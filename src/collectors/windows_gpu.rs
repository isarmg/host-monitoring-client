//! Windows WDDM GPU Engine 的只读 PDH consumer。

use std::{
    collections::BTreeMap,
    fmt,
    mem::{self, MaybeUninit},
    time::Instant,
};

use windows::{
    Win32::System::Performance::{
        PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE,
        PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA, PdhAddEnglishCounterW, PdhCloseQuery,
        PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW,
    },
    core::{PCWSTR, PWSTR, w},
};

use crate::model::{Capability, CapabilityErrorKind, GpuSnapshot};

use super::pdh_buffer::{plan_pdh_buffer, validate_pdh_result};
use super::pdh_recovery::{RetryingPdhResource, should_rebuild_pdh_query};

const ERROR_SUCCESS: u32 = 0;
const MAX_PDH_INSTANCE_NAME_UTF16_UNITS: usize = 1024;

fn physical_engine_key(instance_name: &str) -> String {
    let Some(after_pid) = instance_name.strip_prefix("pid_") else {
        return format!("instance:{instance_name}");
    };
    let Some((pid, after_luid)) = after_pid.split_once("_luid_") else {
        return format!("instance:{instance_name}");
    };
    let Some((luid, after_phys)) = after_luid.split_once("_phys_") else {
        return format!("instance:{instance_name}");
    };
    let Some((phys, after_eng)) = after_phys.split_once("_eng_") else {
        return format!("instance:{instance_name}");
    };
    let Some((engine, engine_type)) = after_eng.split_once("_engtype_") else {
        return format!("instance:{instance_name}");
    };
    let mut luid_parts = luid.split('_');
    let luid_high = luid_parts.next().unwrap_or_default();
    let luid_low = luid_parts.next().unwrap_or_default();
    let valid_hex = |value: &str| {
        value.strip_prefix("0x").is_some_and(|digits| {
            !digits.is_empty()
                && digits.len() <= 16
                && digits.chars().all(|ch| ch.is_ascii_hexdigit())
        })
    };
    let valid_decimal = |value: &str| {
        !value.is_empty()
            && value.len() <= 10
            && value.bytes().all(|byte| byte.is_ascii_digit())
            && value.parse::<u32>().is_ok()
    };
    if !valid_decimal(pid)
        || !valid_hex(luid_high)
        || !valid_hex(luid_low)
        || luid_parts.next().is_some()
        || !valid_decimal(phys)
        || !valid_decimal(engine)
        || engine_type.is_empty()
        || engine_type.len() > 64
        || !engine_type
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return format!("instance:{instance_name}");
    }
    format!(
        "engine:luid_{}_{}_phys_{phys}_eng_{engine}_engtype_{}",
        luid_high.to_ascii_lowercase(),
        luid_low.to_ascii_lowercase(),
        engine_type.to_ascii_lowercase()
    )
}

fn aggregate_engine_utilization(samples: &[(String, f64)]) -> Option<f64> {
    let mut engine_totals = BTreeMap::<String, f64>::new();
    for (instance_name, utilization) in samples {
        if !utilization.is_finite() {
            continue;
        }
        let total = engine_totals
            .entry(physical_engine_key(instance_name))
            .or_default();
        *total = (*total + utilization.clamp(0.0, 100.0)).min(100.0);
    }
    engine_totals
        .into_values()
        .reduce(f64::max)
        .map(|value| value.clamp(0.0, 100.0))
}

#[derive(Debug, Default)]
struct FormattedValuesSummary {
    total_item_count: usize,
    samples: Vec<(String, f64)>,
    invalid_status_count: usize,
    nonfinite_value_count: usize,
}

fn finish_formatted_values(summary: &FormattedValuesSummary) -> (Option<f64>, Capability) {
    if summary.total_item_count == 0 {
        return (
            None,
            Capability::unavailable(
                "gpu.windows.wddm",
                "windows-pdh",
                CapabilityErrorKind::NotPresent,
                "WDDM exposed no GPU Engine instances",
            ),
        );
    }

    let utilization = aggregate_engine_utilization(&summary.samples);
    let rejected_count = summary
        .invalid_status_count
        .saturating_add(summary.nonfinite_value_count);
    if rejected_count == 0 && summary.samples.len() == summary.total_item_count {
        return (
            utilization,
            Capability::available("gpu.windows.wddm", "windows-pdh"),
        );
    }

    let accounted_count = summary.samples.len().saturating_add(rejected_count);
    let unclassified_count = summary.total_item_count.saturating_sub(accounted_count);
    let message = format!(
        "WDDM returned {} GPU Engine instances: {} valid, {} invalid counter status, {} non-finite value, {} unclassified",
        summary.total_item_count,
        summary.samples.len(),
        summary.invalid_status_count,
        summary.nonfinite_value_count,
        unclassified_count,
    );
    (
        utilization,
        Capability::unavailable(
            "gpu.windows.wddm",
            "windows-pdh",
            CapabilityErrorKind::InvalidData,
            message,
        ),
    )
}

fn read_pdh_instance_name(
    name: PWSTR,
    buffer_base: *const u8,
    used_bytes: usize,
    capacity_bytes: usize,
    minimum_name_offset: usize,
) -> Result<String, PdhReadError> {
    if used_bytes > capacity_bytes {
        return Err(PdhReadError::message(format!(
            "PDH instance-name buffer reports {used_bytes} used bytes for {capacity_bytes} bytes of capacity"
        )));
    }
    let base = buffer_base as usize;
    let used_end = base.checked_add(used_bytes).ok_or_else(|| {
        PdhReadError::message("PDH instance-name buffer address overflowed".into())
    })?;
    let capacity_end = base.checked_add(capacity_bytes).ok_or_else(|| {
        PdhReadError::message("PDH instance-name buffer capacity address overflowed".into())
    })?;
    let minimum_name = base.checked_add(minimum_name_offset).ok_or_else(|| {
        PdhReadError::message("PDH instance-name array boundary overflowed".into())
    })?;
    let start = name.0 as usize;
    if name.is_null()
        || start < minimum_name
        || start >= used_end
        || start >= capacity_end
        || !start.is_multiple_of(mem::align_of::<u16>())
    {
        return Err(PdhReadError::message(
            "PDH instance name points outside the live returned name buffer".into(),
        ));
    }
    let available_bytes = (used_end - start).min(capacity_end - start);
    let available_units = available_bytes / mem::size_of::<u16>();
    let scanned_units = available_units.min(MAX_PDH_INSTANCE_NAME_UTF16_UNITS + 1);
    // SAFETY: start was checked for u16 alignment and to be inside both the live returned byte
    // range and allocation capacity. scanned_units is derived from the smaller remaining bound.
    let units = unsafe { std::slice::from_raw_parts(name.0, scanned_units) };
    let Some(nul) = units.iter().position(|unit| *unit == 0) else {
        return Err(PdhReadError::message(format!(
            "PDH instance name is not NUL-terminated within {MAX_PDH_INSTANCE_NAME_UTF16_UNITS} UTF-16 code units"
        )));
    };
    if nul == 0 {
        return Err(PdhReadError::message("PDH instance name is empty".into()));
    }
    String::from_utf16(&units[..nul])
        .map_err(|_| PdhReadError::message("PDH instance name contains invalid UTF-16".into()))
}

pub(super) struct WindowsGpuCollector {
    session: RetryingPdhResource<PdhSession>,
}

impl WindowsGpuCollector {
    pub fn new() -> Self {
        let mut session = RetryingPdhResource::new();
        let _ = session.get_or_try_init(Instant::now(), open_pdh_session);
        Self { session }
    }

    pub fn collect(&mut self) -> (Vec<GpuSnapshot>, Capability) {
        let (query, counter) = match self
            .session
            .get_or_try_init(Instant::now(), open_pdh_session)
        {
            Ok(session) => (session.query, session.counter),
            Err(error) => {
                let error_kind = if self.session.ever_succeeded() {
                    CapabilityErrorKind::Transient
                } else {
                    CapabilityErrorKind::Unsupported
                };
                return (
                    Vec::new(),
                    Capability::unavailable("gpu.windows.wddm", "windows-pdh", error_kind, error),
                );
            }
        };

        // SAFETY: the session owns this query handle until an invalid-handle result causes the
        // session to be dropped below.
        let collect = unsafe { PdhCollectQueryData(query) };
        if collect != ERROR_SUCCESS {
            let message = format!("PdhCollectQueryData returned 0x{collect:08x}");
            if should_rebuild_pdh_query(collect) {
                self.session.invalidate(Instant::now(), message.clone());
            }
            return (
                Vec::new(),
                Capability::unavailable(
                    "gpu.windows.wddm",
                    "windows-pdh",
                    CapabilityErrorKind::Transient,
                    message,
                ),
            );
        }

        match formatted_values(counter) {
            Ok(summary) => {
                let (utilization, capability) = finish_formatted_values(&summary);
                let snapshots = utilization
                    .map(|utilization| GpuSnapshot {
                        id: "windows-wddm".into(),
                        vendor: "unknown".into(),
                        name: "Windows WDDM GPU".into(),
                        utilization_percent: Some(utilization),
                        memory_total_bytes: None,
                        memory_used_bytes: None,
                        temperature_celsius: None,
                        power_watts: None,
                        core_clock_mhz: None,
                        memory_clock_mhz: None,
                        pcie_rx_bytes_per_second: None,
                        pcie_tx_bytes_per_second: None,
                        source: "windows-pdh-gpu-engine".into(),
                    })
                    .into_iter()
                    .collect();
                (snapshots, capability)
            }
            Err(error) => {
                let message = error.to_string();
                if error.status.is_some_and(should_rebuild_pdh_query) {
                    self.session.invalidate(Instant::now(), message.clone());
                }
                (
                    Vec::new(),
                    Capability::unavailable(
                        "gpu.windows.wddm",
                        "windows-pdh",
                        CapabilityErrorKind::Transient,
                        message,
                    ),
                )
            }
        }
    }
}

struct PdhSession {
    query: PDH_HQUERY,
    counter: PDH_HCOUNTER,
}

impl Drop for PdhSession {
    fn drop(&mut self) {
        if !self.query.is_invalid() {
            // SAFETY: this session owns the query and Drop is its final use.
            unsafe { PdhCloseQuery(self.query) };
        }
    }
}

fn open_pdh_session() -> Result<PdhSession, String> {
    let mut query = PDH_HQUERY::default();
    let mut counter = PDH_HCOUNTER::default();
    // SAFETY: PDH writes only the two initialized out handles; the counter path is static
    // UTF-16, and a successfully opened query is owned by the returned session.
    let result = unsafe {
        let open = PdhOpenQueryW(PCWSTR::null(), 0, &mut query);
        if open != ERROR_SUCCESS {
            open
        } else {
            let add = PdhAddEnglishCounterW(
                query,
                w!(r"\GPU Engine(*)\Utilization Percentage"),
                0,
                &mut counter,
            );
            if add == ERROR_SUCCESS {
                let _ = PdhCollectQueryData(query);
            }
            add
        }
    };
    if result == ERROR_SUCCESS {
        Ok(PdhSession { query, counter })
    } else {
        if !query.is_invalid() {
            // SAFETY: query was returned by PdhOpenQueryW and is no longer used.
            unsafe { PdhCloseQuery(query) };
        }
        Err(format!("PDH initialization returned 0x{result:08x}"))
    }
}

#[derive(Debug)]
struct PdhReadError {
    message: String,
    status: Option<u32>,
}

impl PdhReadError {
    fn status(operation: &str, status: u32) -> Self {
        Self {
            message: format!("{operation} returned 0x{status:08x}"),
            status: Some(status),
        }
    }

    fn message(message: String) -> Self {
        Self {
            message,
            status: None,
        }
    }
}

impl fmt::Display for PdhReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

fn formatted_values(counter: PDH_HCOUNTER) -> Result<FormattedValuesSummary, PdhReadError> {
    let mut byte_len = 0_u32;
    let mut item_count = 0_u32;
    // SAFETY: the first call intentionally passes no output buffer to obtain its size.
    let size_result = unsafe {
        PdhGetFormattedCounterArrayW(
            counter,
            PDH_FMT_DOUBLE,
            &mut byte_len,
            &mut item_count,
            None,
        )
    };
    if size_result != PDH_MORE_DATA || byte_len == 0 {
        return if size_result == ERROR_SUCCESS {
            Ok(FormattedValuesSummary {
                total_item_count: usize::try_from(item_count).map_err(|_| {
                    PdhReadError::message(
                        "PDH empty array item count does not fit this platform".into(),
                    )
                })?,
                ..FormattedValuesSummary::default()
            })
        } else {
            Err(PdhReadError::status("PDH array sizing", size_result))
        };
    }

    let item_size = mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
    let layout = plan_pdh_buffer(byte_len, item_count, item_size).map_err(PdhReadError::message)?;
    let mut buffer = Vec::<MaybeUninit<PDH_FMT_COUNTERVALUE_ITEM_W>>::new();
    buffer.try_reserve_exact(layout.slots).map_err(|_| {
        PdhReadError::message(format!(
            "PDH array could not reserve its bounded {} byte buffer",
            layout.capacity_bytes
        ))
    })?;
    buffer.resize_with(layout.slots, MaybeUninit::uninit);
    let pointer = buffer.as_mut_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>();
    byte_len = u32::try_from(layout.capacity_bytes).map_err(|_| {
        PdhReadError::message("PDH array capacity does not fit the API's u32 range".to_string())
    })?;
    // SAFETY: the typed MaybeUninit allocation provides both the byte capacity passed to PDH and
    // the alignment required by PDH_FMT_COUNTERVALUE_ITEM_W. PDH returns item_count initialized
    // entries followed by the instance-name storage in the same backing allocation.
    let result = unsafe {
        PdhGetFormattedCounterArrayW(
            counter,
            PDH_FMT_DOUBLE,
            &mut byte_len,
            &mut item_count,
            Some(pointer),
        )
    };
    if result != ERROR_SUCCESS {
        return Err(PdhReadError::status("PDH array read", result));
    }
    let item_count = validate_pdh_result(layout.capacity_bytes, byte_len, item_count, item_size)
        .map_err(PdhReadError::message)?;
    let used_bytes = usize::try_from(byte_len).map_err(|_| {
        PdhReadError::message("PDH returned byte count does not fit this platform".into())
    })?;
    let minimum_name_offset = item_count.checked_mul(item_size).ok_or_else(|| {
        PdhReadError::message("PDH item array size overflowed while reading names".into())
    })?;

    // SAFETY: a successful PDH call initialized item_count complete entries, and the checked
    // returned byte count proves that those entries fit inside the still-live typed allocation.
    let items = unsafe { std::slice::from_raw_parts(pointer, item_count) };
    let mut summary = FormattedValuesSummary {
        total_item_count: item_count,
        ..FormattedValuesSummary::default()
    };
    for item in items {
        if !matches!(
            item.FmtValue.CStatus,
            PDH_CSTATUS_VALID_DATA | PDH_CSTATUS_NEW_DATA
        ) {
            summary.invalid_status_count += 1;
            continue;
        }
        // SAFETY: PDH_FMT_DOUBLE requests the doubleValue union member.
        let value = unsafe { item.FmtValue.Anonymous.doubleValue };
        if !value.is_finite() {
            summary.nonfinite_value_count += 1;
            continue;
        }
        let name = read_pdh_instance_name(
            item.szName,
            pointer.cast::<u8>(),
            used_bytes,
            layout.capacity_bytes,
            minimum_name_offset,
        )?;
        summary.samples.push((name, value));
    }
    // Keep the backing allocation alive until after all item values have been copied.
    drop(buffer);
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(instance: &str, utilization: f64) -> (String, f64) {
        (instance.to_string(), utilization)
    }

    fn summary(
        total_item_count: usize,
        samples: Vec<(String, f64)>,
        invalid_status_count: usize,
        nonfinite_value_count: usize,
    ) -> FormattedValuesSummary {
        FormattedValuesSummary {
            total_item_count,
            samples,
            invalid_status_count,
            nonfinite_value_count,
        }
    }

    #[test]
    fn empty_formatted_array_is_not_present() {
        let (utilization, capability) = finish_formatted_values(&FormattedValuesSummary::default());

        assert_eq!(utilization, None);
        assert!(!capability.available);
        assert_eq!(capability.error_kind, Some(CapabilityErrorKind::NotPresent));
    }

    #[test]
    fn all_invalid_statuses_are_invalid_data_not_absent() {
        let (utilization, capability) = finish_formatted_values(&summary(2, Vec::new(), 2, 0));

        assert_eq!(utilization, None);
        assert!(!capability.available);
        assert_eq!(
            capability.error_kind,
            Some(CapabilityErrorKind::InvalidData)
        );
        assert!(
            capability
                .message
                .as_deref()
                .is_some_and(|message| message.contains("0 valid, 2 invalid counter status"))
        );
    }

    #[test]
    fn all_nonfinite_values_are_invalid_data_not_absent() {
        let (utilization, capability) = finish_formatted_values(&summary(2, Vec::new(), 0, 2));

        assert_eq!(utilization, None);
        assert!(!capability.available);
        assert_eq!(
            capability.error_kind,
            Some(CapabilityErrorKind::InvalidData)
        );
        assert!(
            capability
                .message
                .as_deref()
                .is_some_and(|message| message.contains("2 non-finite value"))
        );
    }

    #[test]
    fn partial_values_keep_the_snapshot_but_not_full_availability() {
        let valid = sample("pid_1_luid_0x0_0x1_phys_0_eng_0_engtype_3D", 42.0);
        let (utilization, capability) = finish_formatted_values(&summary(3, vec![valid], 1, 1));

        assert_eq!(utilization, Some(42.0));
        assert!(!capability.available);
        assert_eq!(
            capability.error_kind,
            Some(CapabilityErrorKind::InvalidData)
        );
        assert!(
            capability
                .message
                .as_deref()
                .is_some_and(|message| message.contains("1 valid"))
        );
    }

    #[test]
    fn complete_values_are_fully_available() {
        let samples = vec![
            sample("pid_1_luid_0x0_0x1_phys_0_eng_0_engtype_3D", 20.0),
            sample("pid_2_luid_0x0_0x1_phys_0_eng_0_engtype_3D", 30.0),
        ];
        let (utilization, capability) = finish_formatted_values(&summary(2, samples, 0, 0));

        assert_eq!(utilization, Some(50.0));
        assert!(capability.available);
        assert_eq!(capability.error_kind, None);
    }

    #[test]
    fn sums_process_instances_for_the_same_physical_engine() {
        let utilization = aggregate_engine_utilization(&[
            sample(
                "pid_100_luid_0x00000000_0x0000abcd_phys_0_eng_2_engtype_3D",
                35.0,
            ),
            sample(
                "pid_200_luid_0x00000000_0x0000ABCD_phys_0_eng_2_engtype_3d",
                40.0,
            ),
        ]);

        assert_eq!(utilization, Some(75.0));
    }

    #[test]
    fn takes_the_maximum_after_grouping_distinct_engines() {
        let utilization = aggregate_engine_utilization(&[
            sample("pid_100_luid_0x0_0x1_phys_0_eng_0_engtype_3D", 20.0),
            sample("pid_200_luid_0x0_0x1_phys_0_eng_0_engtype_3D", 30.0),
            sample("pid_300_luid_0x0_0x1_phys_0_eng_1_engtype_Copy", 70.0),
        ]);

        assert_eq!(utilization, Some(70.0));
    }

    #[test]
    fn does_not_merge_matching_engine_numbers_from_different_adapters() {
        let utilization = aggregate_engine_utilization(&[
            sample("pid_100_luid_0x0_0xaaaa_phys_0_eng_0_engtype_3D", 60.0),
            sample("pid_200_luid_0x0_0xbbbb_phys_0_eng_0_engtype_3D", 55.0),
        ]);

        assert_eq!(utilization, Some(60.0));
    }

    #[test]
    fn clamps_a_summed_physical_engine_to_one_hundred_percent() {
        let utilization = aggregate_engine_utilization(&[
            sample("pid_100_luid_0x0_0x1_phys_0_eng_0_engtype_3D", 70.0),
            sample("pid_200_luid_0x0_0x1_phys_0_eng_0_engtype_3D", 50.0),
        ]);

        assert_eq!(utilization, Some(100.0));
    }

    #[test]
    fn negative_process_values_do_not_cancel_positive_engine_usage() {
        let utilization = aggregate_engine_utilization(&[
            sample("pid_100_luid_0x0_0x1_phys_0_eng_0_engtype_3D", -80.0),
            sample("pid_200_luid_0x0_0x1_phys_0_eng_0_engtype_3D", 45.0),
        ]);

        assert_eq!(utilization, Some(45.0));
    }

    #[test]
    fn extreme_finite_values_cannot_overflow_an_engine_total() {
        let utilization = aggregate_engine_utilization(&[
            sample("pid_100_luid_0x0_0x1_phys_0_eng_0_engtype_3D", f64::MAX),
            sample("pid_200_luid_0x0_0x1_phys_0_eng_0_engtype_3D", f64::MAX),
        ]);

        let utilization = utilization.unwrap();
        assert!(utilization.is_finite());
        assert_eq!(utilization, 100.0);
    }

    #[test]
    fn isolates_malformed_instance_names_instead_of_dropping_the_pid() {
        let utilization = aggregate_engine_utilization(&[
            sample("pid_bad_luid_0x0_0x1_phys_0_eng_0_engtype_3D", 60.0),
            sample("pid_worse_luid_0x0_0x1_phys_0_eng_0_engtype_3D", 55.0),
        ]);

        assert_eq!(utilization, Some(60.0));
    }

    #[test]
    fn reads_only_nul_terminated_utf16_inside_the_live_buffer_bounds() {
        let mut valid: Vec<u16> = "pid_1_luid_0x0_0x1_phys_0_eng_0_engtype_3D"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let byte_len = valid.len() * mem::size_of::<u16>();
        let name = read_pdh_instance_name(
            PWSTR(valid.as_mut_ptr()),
            valid.as_ptr().cast::<u8>(),
            byte_len,
            byte_len,
            0,
        )
        .unwrap();
        assert_eq!(name, "pid_1_luid_0x0_0x1_phys_0_eng_0_engtype_3D");

        let mut nul_outside_used = vec![b'A' as u16, b'B' as u16, 0];
        let error = read_pdh_instance_name(
            PWSTR(nul_outside_used.as_mut_ptr()),
            nul_outside_used.as_ptr().cast::<u8>(),
            2 * mem::size_of::<u16>(),
            nul_outside_used.len() * mem::size_of::<u16>(),
            0,
        )
        .unwrap_err();
        assert!(error.to_string().contains("not NUL-terminated"));

        let end = unsafe { nul_outside_used.as_mut_ptr().add(nul_outside_used.len()) };
        let error = read_pdh_instance_name(
            PWSTR(end),
            nul_outside_used.as_ptr().cast::<u8>(),
            nul_outside_used.len() * mem::size_of::<u16>(),
            nul_outside_used.len() * mem::size_of::<u16>(),
            0,
        )
        .unwrap_err();
        assert!(error.to_string().contains("outside the live returned"));

        let mut overlong = vec![b'A' as u16; MAX_PDH_INSTANCE_NAME_UTF16_UNITS + 2];
        *overlong.last_mut().unwrap() = 0;
        let byte_len = overlong.len() * mem::size_of::<u16>();
        let error = read_pdh_instance_name(
            PWSTR(overlong.as_mut_ptr()),
            overlong.as_ptr().cast::<u8>(),
            byte_len,
            byte_len,
            0,
        )
        .unwrap_err();
        assert!(error.to_string().contains("not NUL-terminated within"));
    }

    #[test]
    fn rejects_malformed_utf16_and_names_that_overlap_the_item_array() {
        let mut malformed = vec![0xd800, 0];
        let byte_len = malformed.len() * mem::size_of::<u16>();
        let error = read_pdh_instance_name(
            PWSTR(malformed.as_mut_ptr()),
            malformed.as_ptr().cast::<u8>(),
            byte_len,
            byte_len,
            0,
        )
        .unwrap_err();
        assert!(error.to_string().contains("invalid UTF-16"));

        let mut overlapping = vec![b'A' as u16, 0, b'B' as u16, 0];
        let byte_len = overlapping.len() * mem::size_of::<u16>();
        let error = read_pdh_instance_name(
            PWSTR(overlapping.as_mut_ptr()),
            overlapping.as_ptr().cast::<u8>(),
            byte_len,
            byte_len,
            2 * mem::size_of::<u16>(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("outside the live returned"));
    }
}

//! Merge only proven Windows adapter identities. Never infer identity from count or model.
use crate::model::{CLIENT_REPORT_MAX_GPUS, Capability, CapabilityErrorKind, GpuSnapshot};

fn is_luid(id: &str) -> bool {
    let Some(value) = id.strip_prefix("luid_") else {
        return false;
    };
    let Some((high, low)) = value.split_once('_') else {
        return false;
    };
    [high, low]
        .iter()
        .all(|part| part.len() == 8 && part.bytes().all(|c| c.is_ascii_hexdigit()))
}

fn supplement_memory(primary: &mut GpuSnapshot, fallback: &GpuSnapshot) -> bool {
    let fills_used = primary.memory_used_bytes.is_none() && fallback.memory_used_bytes.is_some();
    let fills_total = primary.memory_total_bytes.is_none() && fallback.memory_total_bytes.is_some();
    if !fills_used && !fills_total {
        return true;
    }
    let total = primary.memory_total_bytes.or(fallback.memory_total_bytes);
    let used = primary.memory_used_bytes.or(fallback.memory_used_bytes);
    // Both providers describe dedicated device memory, but reservations or driver
    // reporting may differ. Never manufacture an over-capacity or mismatched pair.
    let different_capacity = fills_used
        && primary.memory_total_bytes.is_some()
        && primary.memory_total_bytes != fallback.memory_total_bytes;
    if different_capacity || matches!((used, total), (Some(used), Some(total)) if used > total) {
        return false;
    }
    primary.memory_total_bytes = total;
    primary.memory_used_bytes = used;
    true
}

pub(super) fn merge_windows(
    gpus: &mut Vec<GpuSnapshot>,
    generic: Vec<GpuSnapshot>,
    nvml_luids: &[Option<&str>],
) -> Option<Capability> {
    let nvml_count = gpus.len();
    // LUIDs are only association keys. Keep the NVML UUID in the wire report so
    // an OS restart does not unnecessarily change the existing OTLP series ID.
    let all_identified = nvml_luids.len() == nvml_count
        && nvml_luids.iter().all(|luid| {
            luid.is_some_and(is_luid)
                && nvml_luids
                    .iter()
                    .filter(|candidate| *candidate == luid)
                    .count()
                    == 1
        });
    let mut withheld = 0;
    let mut incompatible_memory = 0;
    for gpu in &generic {
        if gpu.vendor != "nvidia" || nvml_count == 0 {
            super::push_bounded(gpus, gpu.clone(), CLIENT_REPORT_MAX_GPUS);
            continue;
        }
        let matches: Vec<_> = nvml_luids
            .iter()
            .take(nvml_count)
            .enumerate()
            .filter(|(_, candidate)| **candidate == Some(gpu.id.as_str()))
            .map(|(index, _)| index)
            .collect();
        let unique_generic = generic
            .iter()
            .filter(|candidate| candidate.vendor == "nvidia" && candidate.id == gpu.id)
            .count()
            == 1;
        if is_luid(&gpu.id) && matches.len() == 1 && unique_generic {
            let primary = &mut gpus[matches[0]];
            if !supplement_memory(primary, gpu) {
                incompatible_memory += 1;
            }
            macro_rules! supplement { ($($field:ident),*) => { $(
                if primary.$field.is_none() { primary.$field = gpu.$field; }
            )* }; }
            supplement!(
                utilization_percent,
                temperature_celsius,
                power_watts,
                core_clock_mhz,
                memory_clock_mhz,
                pcie_rx_bytes_per_second,
                pcie_tx_bytes_per_second
            );
            primary.source = "nvml+windows-dxgi-pdh".into();
        } else if all_identified && is_luid(&gpu.id) && matches.is_empty() && unique_generic {
            // Every NVML device has a different proven LUID, so this is an additional adapter.
            super::push_bounded(gpus, gpu.clone(), CLIENT_REPORT_MAX_GPUS);
        } else {
            // Unknown identity might overlap any unmatched reading. Preserve NVML and
            // explicitly disclose the omission instead of sending a false double total.
            withheld += 1;
        }
    }
    (withheld > 0 || incompatible_memory > 0).then(|| Capability::unavailable(
        if withheld > 0 { "gpu.nvidia.identity" } else { "gpu.nvidia.memory" },
        "cuda-driver/dxgi", CapabilityErrorKind::Transient,
        format!("Skipped {withheld} DXGI NVIDIA readings with uncertain identity and {incompatible_memory} incompatible memory supplements; NVML readings and proven compatible fields are retained"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn merge_windows(gpus: &mut Vec<GpuSnapshot>, generic: Vec<GpuSnapshot>) -> Option<Capability> {
        let ids: Vec<_> = gpus.iter().map(|gpu| gpu.id.clone()).collect();
        let luids: Vec<_> = ids
            .iter()
            .map(|id| is_luid(id).then_some(id.as_str()))
            .collect();
        super::merge_windows(gpus, generic, &luids)
    }

    fn gpu(id: &str, source: &str, used: Option<u64>) -> GpuSnapshot {
        GpuSnapshot {
            id: id.into(),
            vendor: "nvidia".into(),
            name: "same NVIDIA model".into(),
            utilization_percent: Some(50.0),
            memory_total_bytes: Some(1000),
            memory_used_bytes: used,
            temperature_celsius: Some(60.0),
            power_watts: None,
            core_clock_mhz: None,
            memory_clock_mhz: None,
            pcie_rx_bytes_per_second: None,
            pcie_tx_bytes_per_second: None,
            source: source.into(),
        }
    }
    #[test]
    fn partial_enumeration_merges_one_identity_and_keeps_the_other_device() {
        let mut nvml = vec![gpu("luid_00000000_00000001", "nvml", Some(900))];
        assert!(
            merge_windows(
                &mut nvml,
                vec![
                    gpu("luid_00000000_00000001", "windows-dxgi-pdh", Some(900)),
                    gpu("luid_00000000_00000002", "windows-dxgi-pdh", Some(0))
                ]
            )
            .is_none()
        );
        assert_eq!(nvml.len(), 2);
        assert_eq!(
            nvml.iter().filter_map(|g| g.memory_used_bytes).sum::<u64>(),
            900
        );
        assert_eq!(
            nvml.iter()
                .filter_map(|g| g.memory_total_bytes)
                .sum::<u64>(),
            2000
        );
        assert_eq!(nvml[0].source, "nvml+windows-dxgi-pdh");
    }
    #[test]
    fn matching_identity_supplements_missing_fields_and_retains_native_values() {
        let mut primary = gpu("luid_00000000_00000001", "nvml", None);
        primary.memory_total_bytes = None;
        primary.utilization_percent = None;
        primary.temperature_celsius = Some(72.0);
        let mut nvml = vec![primary];
        merge_windows(
            &mut nvml,
            vec![gpu("luid_00000000_00000001", "windows-dxgi-pdh", Some(900))],
        );
        assert_eq!(nvml.len(), 1);
        assert_eq!(nvml[0].memory_used_bytes, Some(900));
        assert_eq!(nvml[0].utilization_percent, Some(50.0));
        assert_eq!(nvml[0].temperature_celsius, Some(72.0));
    }
    #[test]
    fn unknown_identity_does_not_prevent_proven_matches_or_fabricate_a_union() {
        let mut known = gpu("luid_00000000_00000001", "nvml", None);
        known.utilization_percent = None;
        let mut nvml = vec![known, gpu("GPU-unmatched", "nvml", Some(900))];
        let diagnostic = merge_windows(
            &mut nvml,
            vec![
                gpu("luid_00000000_00000001", "windows-dxgi-pdh", Some(100)),
                gpu("luid_00000000_00000002", "windows-dxgi-pdh", Some(900)),
            ],
        )
        .unwrap();
        assert!(!diagnostic.available);
        assert_eq!(nvml.len(), 2);
        assert_eq!(nvml[0].memory_used_bytes, Some(100));
        assert_eq!(nvml[1].id, "GPU-unmatched");
        assert_eq!(nvml[1].memory_used_bytes, Some(900));
    }
    #[test]
    fn no_nvml_keeps_generic_data_and_duplicate_identities_are_not_guessed() {
        let mut empty = vec![];
        assert!(
            merge_windows(
                &mut empty,
                vec![gpu("luid_00000000_00000001", "windows-dxgi-pdh", Some(900))]
            )
            .is_none()
        );
        assert_eq!(empty.len(), 1);
        let mut nvml = vec![gpu("luid_00000000_00000001", "nvml", Some(900))];
        let generic = vec![gpu("luid_00000000_00000001", "windows-dxgi-pdh", Some(1)); 2];
        assert!(merge_windows(&mut nvml, generic).is_some());
        assert_eq!(nvml.len(), 1);
        assert_eq!(nvml[0].memory_used_bytes, Some(900));
    }

    #[test]
    fn equal_counts_and_models_do_not_hide_a_proven_different_device() {
        let mut nvml = vec![gpu("luid_00000000_00000001", "nvml", Some(900))];
        assert!(
            merge_windows(
                &mut nvml,
                vec![gpu("luid_00000000_00000002", "windows-dxgi-pdh", Some(0))]
            )
            .is_none()
        );
        assert_eq!(nvml.len(), 2);
        assert_ne!(nvml[0].id, nvml[1].id);
    }

    #[test]
    fn matching_luid_preserves_the_stable_nvml_uuid_in_the_report() {
        let uuid = "GPU-00112233-4455-6677-8899-aabbccddeeff";
        let mut nvml = vec![gpu(uuid, "nvml", None)];
        assert!(
            super::merge_windows(
                &mut nvml,
                vec![gpu("luid_00000000_00000001", "windows-dxgi-pdh", Some(900))],
                &[Some("luid_00000000_00000001")],
            )
            .is_none()
        );
        assert_eq!(nvml.len(), 1);
        assert_eq!(nvml[0].id, uuid);
        assert_eq!(nvml[0].memory_used_bytes, Some(900));
    }

    #[test]
    fn incompatible_memory_sources_do_not_manufacture_a_full_device() {
        for (total, used) in [
            (Some(2000), Some(1500)),
            (Some(2000), Some(900)),
            (None, Some(900)),
        ] {
            let mut nvml = vec![gpu("luid_00000000_00000001", "nvml", None)];
            nvml[0].utilization_percent = None;
            let mut fallback = gpu("luid_00000000_00000001", "windows-dxgi-pdh", used);
            fallback.memory_total_bytes = total;
            let diagnostic = merge_windows(&mut nvml, vec![fallback]).unwrap();
            assert!(!diagnostic.available);
            assert_eq!(nvml[0].memory_total_bytes, Some(1000));
            assert_eq!(nvml[0].memory_used_bytes, None);
            assert_eq!(nvml[0].utilization_percent, Some(50.0));
        }
        let mut nvml = vec![gpu("luid_00000000_00000001", "nvml", None)];
        nvml[0].memory_total_bytes = None;
        assert!(
            merge_windows(
                &mut nvml,
                vec![gpu(
                    "luid_00000000_00000001",
                    "windows-dxgi-pdh",
                    Some(1500)
                )]
            )
            .is_some()
        );
        assert_eq!(nvml[0].memory_total_bytes, None);
        assert_eq!(nvml[0].memory_used_bytes, None);
    }
}

//! DMTF SMBIOS Type 17. All offsets are checked against each record's formatted length.
use crate::model::{CapabilityErrorKind, MAX_MEMORY_MODULES, MemoryModule};

pub(super) fn memory_type(value: u64) -> Option<String> {
    Some(
        match value {
            3 => "DRAM",
            15 => "SDRAM",
            18 => "DDR",
            19 => "DDR2",
            20 => "DDR2 FB-DIMM",
            24 => "DDR3",
            25 => "FBD2",
            26 => "DDR4",
            27 => "LPDDR",
            28 => "LPDDR2",
            29 => "LPDDR3",
            30 => "LPDDR4",
            32 => "HBM",
            33 => "HBM2",
            34 => "DDR5",
            35 => "LPDDR5",
            36 => "HBM3",
            _ => return None,
        }
        .into(),
    )
}

pub(super) fn form_factor(value: u64) -> Option<String> {
    Some(
        match value {
            3 => "SIMM",
            4 => "SIP",
            5 => "Chip",
            6 => "DIP",
            7 => "ZIP",
            8 => "Proprietary Card",
            9 => "DIMM",
            10 => "TSOP",
            11 => "Row of chips",
            12 => "RIMM",
            13 => "SO-DIMM",
            14 => "SRIMM",
            15 => "FB-DIMM",
            16 => "Die",
            17 => "CAMM",
            _ => return None,
        }
        .into(),
    )
}

pub(super) fn firmware_text(value: &str) -> Option<String> {
    let value = super::super::hardware::text(value)?;
    if matches!(
        value.to_ascii_lowercase().as_str(),
        "unknown"
            | "not specified"
            | "not available"
            | "none"
            | "n/a"
            | "other"
            | "to be filled by o.e.m."
            | "default string"
    ) {
        None
    } else {
        Some(value)
    }
}

fn word(record: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        record.get(offset..offset + 2)?.try_into().ok()?,
    ))
}
fn dword(record: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        record.get(offset..offset + 4)?.try_into().ok()?,
    ))
}
fn speed(record: &[u8], offset: usize, extended: usize) -> Option<f64> {
    match word(record, offset)? {
        0 => None,
        0xffff => dword(record, extended)
            .filter(|v| *v > 0 && *v <= 0x7fff_ffff)
            .map(f64::from),
        value => Some(f64::from(value)),
    }
}

pub(super) fn parse(bytes: &[u8]) -> (Vec<MemoryModule>, Option<CapabilityErrorKind>) {
    let mut modules = Vec::new();
    let mut cursor = 0;
    let mut failure = None;
    while cursor < bytes.len() {
        let Some(header) = bytes.get(cursor..cursor + 4) else {
            failure = Some(CapabilityErrorKind::InvalidData);
            break;
        };
        let length = header[1] as usize;
        if length < 4 {
            failure = Some(CapabilityErrorKind::InvalidData);
            break;
        }
        let Some(record) = bytes.get(cursor..cursor + length) else {
            failure = Some(CapabilityErrorKind::InvalidData);
            break;
        };
        let tail = &bytes[cursor + length..];
        let Some(end) = tail.windows(2).position(|w| w == [0, 0]) else {
            failure = Some(CapabilityErrorKind::InvalidData);
            break;
        };
        if header[0] == 127 {
            break;
        }
        if header[0] == 17 {
            if length < 0x15 {
                failure = Some(CapabilityErrorKind::InvalidData);
            } else if word(record, 0x0c) != Some(0) && record[0x12] != 0x1f {
                let strings: Vec<_> = tail[..end].split(|b| *b == 0).collect();
                let string = |offset: usize| -> Option<String> {
                    let index = usize::from(*record.get(offset)?).checked_sub(1)?;
                    firmware_text(&String::from_utf8_lossy(strings.get(index)?))
                };
                let capacity_bytes = match word(record, 0x0c) {
                    Some(0xffff) | None => None,
                    Some(0x7fff) => dword(record, 0x1c)
                        .filter(|v| *v & 0x8000_0000 == 0 && *v > 0)
                        .map(|v| u64::from(v) * 1024 * 1024),
                    Some(v) if v & 0x8000 != 0 => Some(u64::from(v & 0x7fff) * 1024),
                    Some(v) => Some(u64::from(v) * 1024 * 1024),
                };
                modules.push(MemoryModule {
                    id: format!("smbios:{:04x}", word(record, 2).unwrap_or(0)),
                    locator: string(0x10),
                    model: string(0x1a),
                    vendor: string(0x17),
                    memory_type: memory_type(u64::from(record[0x12])),
                    module_version: string(0x2b),
                    form_factor: form_factor(u64::from(record[0x0e])),
                    capacity_bytes,
                    speed_mt_s: speed(record, 0x15, 0x54),
                    configured_speed_mt_s: speed(record, 0x20, 0x58),
                    reported_speed: None,
                    source: "linux-smbios".into(),
                });
                if modules.len() == MAX_MEMORY_MODULES {
                    break;
                }
            }
        }
        cursor += length + end + 2;
    }
    modules.sort_by(|a, b| a.id.cmp(&b.id));
    modules.dedup_by(|a, b| a.id == b.id);
    (modules, failure)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record(size: u16) -> Vec<u8> {
        let mut bytes = vec![0; 0x5c];
        bytes[0] = 17;
        bytes[1] = 0x5c;
        bytes[2] = 42;
        bytes[0x0c..0x0e].copy_from_slice(&size.to_le_bytes());
        bytes[0x0e] = 9;
        bytes[0x10] = 1;
        bytes[0x12] = 34;
        bytes[0x17] = 2;
        bytes[0x1a] = 3;
        bytes[0x2b] = 4;
        bytes[0x15..0x17].copy_from_slice(&5600u16.to_le_bytes());
        bytes[0x20..0x22].copy_from_slice(&4800u16.to_le_bytes());
        bytes.extend_from_slice(b"DIMM A1\0Samsung\0M323R2GA3BB0\0FW1.2\0\0");
        bytes
    }
    #[test]
    fn ddr5_part_vendor_capacity_and_configured_rate_are_separate() {
        let (modules, failure) = parse(&record(16384));
        assert_eq!(failure, None);
        assert_eq!(modules.len(), 1);
        let m = &modules[0];
        assert_eq!(m.memory_type.as_deref(), Some("DDR5"));
        assert_eq!(m.form_factor.as_deref(), Some("DIMM"));
        assert_eq!(m.vendor.as_deref(), Some("Samsung"));
        assert_eq!(m.model.as_deref(), Some("M323R2GA3BB0"));
        assert_eq!(m.module_version.as_deref(), Some("FW1.2"));
        assert_eq!(m.capacity_bytes, Some(16 * 1024 * 1024 * 1024));
        assert_eq!(m.speed_mt_s, Some(5600.0));
        assert_eq!(m.configured_speed_mt_s, Some(4800.0));
        assert!(parse(&record(0)).0.is_empty());
    }
    #[test]
    fn extended_size_and_speed_sentinels_and_truncated_tables() {
        let mut bytes = record(0x7fff);
        bytes[0x1c..0x20].copy_from_slice(&65536u32.to_le_bytes());
        bytes[0x15..0x17].copy_from_slice(&0xffffu16.to_le_bytes());
        bytes[0x54..0x58].copy_from_slice(&100000u32.to_le_bytes());
        let modules = parse(&bytes).0;
        assert_eq!(modules[0].capacity_bytes, Some(64 * 1024 * 1024 * 1024));
        assert_eq!(modules[0].speed_mt_s, Some(100000.0));
        assert_eq!(parse(&record(0xffff)).0[0].capacity_bytes, None);
        for len in 1..0x5c {
            assert_eq!(
                parse(&bytes[..len]).1,
                Some(CapabilityErrorKind::InvalidData)
            );
        }
        let mut truncated = bytes;
        truncated.extend_from_slice(&[17, 2, 0, 0]);
        let (readable, failure) = parse(&truncated);
        assert_eq!(readable.len(), 1);
        assert_eq!(failure, Some(CapabilityErrorKind::InvalidData));
    }
}

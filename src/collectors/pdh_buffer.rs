//! Resource and layout bounds for PDH's caller-owned counter array buffer.

pub(super) const MAX_PDH_ARRAY_BYTES: usize = 4 * 1024 * 1024;
pub(super) const MAX_PDH_ARRAY_ITEMS: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PdhBufferLayout {
    pub slots: usize,
    pub capacity_bytes: usize,
}

pub(super) fn plan_pdh_buffer(
    required_bytes: u32,
    item_count: u32,
    item_size: usize,
) -> Result<PdhBufferLayout, String> {
    if item_size == 0 {
        return Err("PDH item type has zero size".into());
    }
    let required_bytes = usize::try_from(required_bytes)
        .map_err(|_| "PDH required byte count does not fit this platform".to_string())?;
    if required_bytes == 0 {
        return Err("PDH returned a zero-sized array buffer".into());
    }
    if required_bytes > MAX_PDH_ARRAY_BYTES {
        return Err(format!(
            "PDH requested {required_bytes} array bytes, exceeding the {} byte limit",
            MAX_PDH_ARRAY_BYTES
        ));
    }
    let item_count = usize::try_from(item_count)
        .map_err(|_| "PDH item count does not fit this platform".to_string())?;
    if item_count > MAX_PDH_ARRAY_ITEMS {
        return Err(format!(
            "PDH returned {item_count} array items, exceeding the {MAX_PDH_ARRAY_ITEMS} item limit"
        ));
    }
    let minimum_item_bytes = item_count
        .checked_mul(item_size)
        .ok_or_else(|| "PDH item array size overflowed".to_string())?;
    if minimum_item_bytes > required_bytes {
        return Err(format!(
            "PDH byte count {required_bytes} cannot contain {item_count} array items"
        ));
    }

    let slots = required_bytes
        .checked_add(item_size - 1)
        .ok_or_else(|| "PDH buffer rounding overflowed".to_string())?
        / item_size;
    let capacity_bytes = slots
        .checked_mul(item_size)
        .ok_or_else(|| "PDH buffer capacity overflowed".to_string())?;
    if capacity_bytes > MAX_PDH_ARRAY_BYTES {
        return Err(format!(
            "PDH aligned buffer requires {capacity_bytes} bytes, exceeding the {} byte limit",
            MAX_PDH_ARRAY_BYTES
        ));
    }
    u32::try_from(capacity_bytes)
        .map_err(|_| "PDH buffer capacity exceeds the API's u32 range".to_string())?;

    Ok(PdhBufferLayout {
        slots,
        capacity_bytes,
    })
}

pub(super) fn validate_pdh_result(
    capacity_bytes: usize,
    used_bytes: u32,
    item_count: u32,
    item_size: usize,
) -> Result<usize, String> {
    if item_size == 0 {
        return Err("PDH item type has zero size".into());
    }
    if capacity_bytes > MAX_PDH_ARRAY_BYTES {
        return Err("PDH buffer capacity exceeds the configured resource limit".into());
    }
    let used_bytes = usize::try_from(used_bytes)
        .map_err(|_| "PDH used byte count does not fit this platform".to_string())?;
    if used_bytes > capacity_bytes {
        return Err(format!(
            "PDH reported {used_bytes} used bytes for a {capacity_bytes} byte buffer"
        ));
    }
    let item_count = usize::try_from(item_count)
        .map_err(|_| "PDH item count does not fit this platform".to_string())?;
    if item_count > MAX_PDH_ARRAY_ITEMS {
        return Err(format!(
            "PDH returned {item_count} array items, exceeding the {MAX_PDH_ARRAY_ITEMS} item limit"
        ));
    }
    let minimum_item_bytes = item_count
        .checked_mul(item_size)
        .ok_or_else(|| "PDH item array size overflowed".to_string())?;
    if minimum_item_bytes > used_bytes {
        return Err(format!(
            "PDH used byte count {used_bytes} cannot contain {item_count} array items"
        ));
    }
    Ok(item_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ITEM_SIZE: usize = 24;

    #[test]
    fn planning_rounds_up_to_typed_slots() {
        assert_eq!(
            plan_pdh_buffer(25, 1, ITEM_SIZE).unwrap(),
            PdhBufferLayout {
                slots: 2,
                capacity_bytes: 48,
            }
        );
    }

    #[test]
    fn planning_enforces_independent_byte_and_item_limits() {
        let aligned_max = (MAX_PDH_ARRAY_BYTES / ITEM_SIZE) * ITEM_SIZE;
        assert!(
            plan_pdh_buffer(
                u32::try_from(aligned_max).unwrap(),
                u32::try_from(MAX_PDH_ARRAY_ITEMS).unwrap(),
                ITEM_SIZE,
            )
            .is_ok()
        );
        assert!(
            plan_pdh_buffer(
                u32::try_from(MAX_PDH_ARRAY_BYTES + 1).unwrap(),
                0,
                ITEM_SIZE,
            )
            .is_err()
        );
        assert!(
            plan_pdh_buffer(
                u32::try_from(MAX_PDH_ARRAY_ITEMS * ITEM_SIZE).unwrap(),
                u32::try_from(MAX_PDH_ARRAY_ITEMS + 1).unwrap(),
                ITEM_SIZE,
            )
            .is_err()
        );
    }

    #[test]
    fn planning_rejects_rounding_and_layout_overflow() {
        assert!(plan_pdh_buffer(0, 0, ITEM_SIZE).is_err());
        let unaligned_limit = u32::try_from(MAX_PDH_ARRAY_BYTES).unwrap();
        assert!(
            plan_pdh_buffer(unaligned_limit, 0, ITEM_SIZE).is_err(),
            "rounding a typed allocation must not exceed the byte limit"
        );
        assert!(plan_pdh_buffer(1, 2, usize::MAX).is_err());
        assert!(plan_pdh_buffer(1, 1, 0).is_err());
        assert!(plan_pdh_buffer(1, 1, ITEM_SIZE).is_err());
    }

    #[test]
    fn successful_result_must_fit_the_actual_returned_bytes() {
        assert_eq!(validate_pdh_result(48, 48, 2, ITEM_SIZE).unwrap(), 2);
        assert_eq!(
            validate_pdh_result(72, 72, 3, ITEM_SIZE).unwrap(),
            3,
            "the instance list may grow between PDH's sizing and read calls"
        );
        assert_eq!(validate_pdh_result(48, 0, 0, ITEM_SIZE).unwrap(), 0);
        assert!(validate_pdh_result(48, 0, 1, ITEM_SIZE).is_err());
        assert!(validate_pdh_result(48, 49, 2, ITEM_SIZE).is_err());
        assert!(validate_pdh_result(48, 47, 2, ITEM_SIZE).is_err());
        assert!(
            validate_pdh_result(
                MAX_PDH_ARRAY_BYTES,
                u32::try_from(MAX_PDH_ARRAY_BYTES).unwrap(),
                u32::try_from(MAX_PDH_ARRAY_ITEMS + 1).unwrap(),
                ITEM_SIZE,
            )
            .is_err()
        );
        assert!(validate_pdh_result(MAX_PDH_ARRAY_BYTES + 1, 0, 0, ITEM_SIZE).is_err());
        assert!(validate_pdh_result(1, 0, 0, 0).is_err());
    }
}

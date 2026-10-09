//! Read-only display metadata for launchd accounts outside the graphical session.
use super::{Inventory, device};
use crate::model::{Capability, HardwareDevice, HardwareDeviceKind, MAX_HARDWARE_DEVICES};
use std::ffi::{CStr, c_char, c_void};

type Ref = *const c_void;
#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const c_char) -> Ref;
    fn IOServiceGetMatchingServices(port: u32, matching: Ref, iterator: *mut u32) -> i32;
    fn IOIteratorNext(iterator: u32) -> u32;
    fn IOObjectRelease(object: u32) -> i32;
    fn IORegistryEntryGetPath(entry: u32, plane: *const c_char, path: *mut c_char) -> i32;
    fn IORegistryEntryCreateCFProperty(entry: u32, key: Ref, allocator: Ref, options: u32) -> Ref;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFGetTypeID(value: Ref) -> usize;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFDictionaryGetValue(dictionary: Ref, key: Ref) -> Ref;
    fn CFStringGetTypeID() -> usize;
    fn CFStringCreateWithCString(allocator: Ref, string: *const c_char, encoding: u32) -> Ref;
    fn CFStringGetCString(string: Ref, buffer: *mut c_char, size: isize, encoding: u32) -> u8;
    fn CFNumberGetTypeID() -> usize;
    fn CFNumberGetValue(number: Ref, kind: isize, value: *mut c_void) -> u8;
    fn CFRelease(value: Ref);
}
struct OwnedRef(Ref);
impl Drop for OwnedRef {
    fn drop(&mut self) {
        // SAFETY: this non-null Create result owns exactly one reference.
        unsafe { CFRelease(self.0) }
    }
}
struct IoObject(u32);
impl Drop for IoObject {
    fn drop(&mut self) {
        // SAFETY: iterator and entry handles each own one reference.
        unsafe { IOObjectRelease(self.0) };
    }
}
fn key(name: &CStr) -> Option<OwnedRef> {
    // SAFETY: name is NUL-terminated; null selects the default allocator.
    let raw = unsafe { CFStringCreateWithCString(std::ptr::null(), name.as_ptr(), 0x08000100) };
    (!raw.is_null()).then_some(OwnedRef(raw))
}
// The borrowed properties below never escape the owning attributes reference.
fn field(dictionary: Ref, name: &CStr) -> Option<Ref> {
    // SAFETY: callers pass live CF objects; check the type before dictionary access.
    if dictionary.is_null() || unsafe { CFGetTypeID(dictionary) != CFDictionaryGetTypeID() } {
        return None;
    }
    let key = key(name)?;
    // SAFETY: dictionary has the checked type and key is a live CFString.
    let value = unsafe { CFDictionaryGetValue(dictionary, key.0) };
    (!value.is_null()).then_some(value)
}
fn string(value: Ref) -> Option<String> {
    // SAFETY: value is a borrowed, live property; only strings are converted.
    if unsafe { CFGetTypeID(value) != CFStringGetTypeID() } {
        return None;
    }
    let mut buffer = [0_u8; 1024];
    // SAFETY: fixed writable buffer, UTF-8 encoding; conversion is bounded.
    if unsafe {
        CFStringGetCString(
            value,
            buffer.as_mut_ptr().cast(),
            buffer.len() as isize,
            0x08000100,
        )
    } == 0
    {
        return None;
    }
    let end = buffer.iter().position(|byte| *byte == 0)?;
    super::super::hardware::text(std::str::from_utf8(&buffer[..end]).ok()?)
}
fn numeric_id(value: Ref) -> Option<String> {
    // SAFETY: value is a live property; kCFNumberSInt64Type (4) writes one i64.
    if unsafe { CFGetTypeID(value) != CFNumberGetTypeID() } {
        return None;
    }
    let mut number = 0_i64;
    if unsafe { CFNumberGetValue(value, 4, (&mut number as *mut i64).cast()) } == 0
        || !(0..=u16::MAX as i64).contains(&number)
    {
        return None;
    }
    Some(format!("0x{number:04x}"))
}
fn displays() -> Vec<HardwareDevice> {
    // SAFETY: matching consumes this dictionary, including on failure; port 0 is
    // the documented default IOKit main port. No device is opened or modified.
    let matching = unsafe { IOServiceMatching(c"IOMobileFramebuffer".as_ptr()) };
    if matching.is_null() {
        return vec![];
    }
    let mut iterator = 0;
    if unsafe { IOServiceGetMatchingServices(0, matching, &mut iterator) } != 0 || iterator == 0 {
        return vec![];
    }
    let iterator = IoObject(iterator);
    let Some(attributes_key) = key(c"DisplayAttributes") else {
        return vec![];
    };
    let mut devices = Vec::new();
    for _ in 0..64 {
        // SAFETY: iterator is owned and alive; zero marks the end.
        let entry = unsafe { IOIteratorNext(iterator.0) };
        if entry == 0 {
            break;
        }
        let entry = IoObject(entry);
        // SAFETY: Create returns an owned snapshot; only allowlisted fields are read.
        let raw = unsafe {
            IORegistryEntryCreateCFProperty(entry.0, attributes_key.0, std::ptr::null(), 0)
        };
        if raw.is_null() {
            continue;
        }
        let attributes = OwnedRef(raw);
        let Some(product) = field(attributes.0, c"ProductAttributes") else {
            continue;
        };
        let Some(name) = field(product, c"ProductName").and_then(string) else {
            continue;
        };
        let mut path = [0_u8; 512];
        // SAFETY: IOKit io_string_t is a 512-byte buffer; path belongs to this entry.
        if unsafe {
            IORegistryEntryGetPath(entry.0, c"IOService".as_ptr(), path.as_mut_ptr().cast())
        } != 0
        {
            continue;
        }
        let Some(end) = path.iter().position(|byte| *byte == 0) else {
            continue;
        };
        let Ok(path) = std::str::from_utf8(&path[..end]) else {
            continue;
        };
        let mut item = device(
            &format!("iokit:{path}"),
            HardwareDeviceKind::Monitor,
            &name,
            "macos-iokit-display",
        );
        item.model = Some(name);
        item.vendor_id = field(product, c"LegacyManufacturerID").and_then(numeric_id);
        item.product_id = field(product, c"ProductID").and_then(numeric_id);
        devices.push(item);
    }
    devices.sort_by(|a, b| a.id.cmp(&b.id));
    devices.dedup_by(|a, b| a.id == b.id);
    devices
}
pub(super) fn complete(inventory: Inventory) -> Inventory {
    complete_with(inventory, displays)
}
fn complete_with(
    mut inventory: Inventory,
    read: impl FnOnce() -> Vec<HardwareDevice>,
) -> Inventory {
    // The graphical session report remains authoritative when it exposes displays.
    if inventory
        .devices
        .iter()
        .any(|d| d.kind == HardwareDeviceKind::Monitor)
    {
        return inventory;
    }
    let remaining = MAX_HARDWARE_DEVICES.saturating_sub(inventory.devices.len());
    let displays = read();
    if displays.is_empty() || remaining == 0 {
        return inventory;
    }
    inventory
        .devices
        .extend(displays.into_iter().take(remaining));
    inventory.collected_at.get_or_insert_with(chrono::Utc::now);
    if let Some(capability) = inventory
        .capabilities
        .iter_mut()
        .find(|c| c.name == "hardware.monitors")
    {
        *capability = Capability::available("hardware.monitors", "macos-iokit-display");
    }
    inventory
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CapabilityErrorKind;
    #[test]
    fn display_fallback_preserves_reported_devices_and_other_failures() {
        let inventory = Inventory::empty(CapabilityErrorKind::Transient, "report unavailable");
        let display = device(
            "iokit:display",
            HardwareDeviceKind::Monitor,
            "HDP-V104",
            "macos-iokit-display",
        );
        let filled = complete_with(inventory, || vec![display]);
        assert!(filled.collected_at.is_some());
        assert_eq!(filled.devices.len(), 1);
        assert!(
            filled
                .capabilities
                .iter()
                .find(|c| c.name == "hardware.monitors")
                .unwrap()
                .available
        );
        assert!(
            filled
                .capabilities
                .iter()
                .filter(|c| c.name != "hardware.monitors")
                .all(|c| c.error_kind == Some(CapabilityErrorKind::Transient))
        );
        let unchanged = complete_with(filled, || {
            panic!("already reported display must not be duplicated")
        });
        assert_eq!(unchanged.devices.len(), 1);
    }
}

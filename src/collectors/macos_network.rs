//! Public SystemConfiguration inventory and BSD interface flags; no command parsing.
use crate::model::{MAX_HARDWARE_NETWORKS, PhysicalNetworkAdapter};
use std::{
    collections::HashMap,
    ffi::{CStr, c_char, c_void},
};

type Ref = *const c_void;
#[link(name = "SystemConfiguration", kind = "framework")]
unsafe extern "C" {
    fn SCNetworkInterfaceCopyAll() -> Ref;
    fn SCNetworkInterfaceGetBSDName(interface: Ref) -> Ref;
    fn SCNetworkInterfaceGetInterfaceType(interface: Ref) -> Ref;
    fn SCNetworkInterfaceGetLocalizedDisplayName(interface: Ref) -> Ref;
    fn SCNetworkInterfaceGetHardwareAddressString(interface: Ref) -> Ref;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFArrayGetCount(array: Ref) -> isize;
    fn CFArrayGetValueAtIndex(array: Ref, index: isize) -> Ref;
    fn CFStringGetCString(string: Ref, buffer: *mut c_char, size: isize, encoding: u32) -> u8;
    fn CFRelease(value: Ref);
}
struct OwnedRef(Ref);
impl Drop for OwnedRef {
    fn drop(&mut self) {
        // SAFETY: a non-null Copy result owns one reference; this is its only release.
        unsafe { CFRelease(self.0) }
    }
}
fn string(value: Ref) -> Option<String> {
    if value.is_null() {
        return None;
    }
    let mut buffer = [0_u8; 1024];
    // SAFETY: callers provide CFStringRefs from SC getters, alive with the array. The
    // UTF-8 conversion writes a NUL terminator inside the supplied buffer on success.
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
    super::physical_network::clean_text(std::str::from_utf8(&buffer[..end]).ok()?)
}
pub(super) fn physical_adapters() -> Vec<PhysicalNetworkAdapter> {
    // SAFETY: this public API takes no parameters and returns an owned array or null.
    let raw = unsafe { SCNetworkInterfaceCopyAll() };
    if raw.is_null() {
        return Vec::new();
    }
    let array = OwnedRef(raw);
    // SAFETY: array owns the returned CFArray for the entire enumeration.
    let count = unsafe { CFArrayGetCount(array.0) }.clamp(0, 4096);
    let mut adapters = Vec::new();
    for index in 0..count {
        // SAFETY: index is inside the array bounds; each value is an SCNetworkInterfaceRef.
        let interface = unsafe { CFArrayGetValueAtIndex(array.0, index) };
        if interface.is_null() {
            continue;
        }
        // SAFETY: getters borrow the interface from our live array and return CFStringRefs.
        let (kind, name, label, mac) = unsafe {
            (
                string(SCNetworkInterfaceGetInterfaceType(interface)),
                string(SCNetworkInterfaceGetBSDName(interface)),
                string(SCNetworkInterfaceGetLocalizedDisplayName(interface)),
                string(SCNetworkInterfaceGetHardwareAddressString(interface)),
            )
        };
        if !matches!(kind.as_deref(), Some("Ethernet" | "IEEE80211")) {
            continue;
        }
        let Some(name) = name else {
            continue;
        };
        if name.len() >= libc::IFNAMSIZ {
            continue;
        }
        adapters.push(PhysicalNetworkAdapter {
            id: format!("scnetwork:{name}"),
            name: label.unwrap_or_else(|| name.clone()),
            interface_name: Some(name),
            mac_address: mac
                .as_deref()
                .and_then(super::physical_network::normalized_mac),
            link_speed_mbps: None,
            source: "macos-system-configuration".into(),
        });
        if adapters.len() == MAX_HARDWARE_NETWORKS {
            break;
        }
    }
    adapters.sort_by(|a, b| a.id.cmp(&b.id));
    adapters.dedup_by(|a, b| a.id == b.id);
    adapters
}
pub(super) fn interface_states() -> HashMap<String, String> {
    let mut head = std::ptr::null_mut();
    // SAFETY: getifaddrs initializes this output; the allocation is released below.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return HashMap::new();
    }
    struct Addresses(*mut libc::ifaddrs);
    impl Drop for Addresses {
        fn drop(&mut self) {
            // SAFETY: this is the original allocation returned by getifaddrs.
            unsafe { libc::freeifaddrs(self.0) }
        }
    }
    let addresses = Addresses(head);
    let mut current = addresses.0;
    let mut states = HashMap::new();
    for _ in 0..4096 {
        if current.is_null() {
            break;
        }
        // SAFETY: each node and its NUL-terminated name belong to the live allocation.
        let item = unsafe { &*current };
        if !item.ifa_name.is_null() {
            // SAFETY: getifaddrs guarantees an interface name for this non-null pointer.
            if let Ok(name) = unsafe { CStr::from_ptr(item.ifa_name) }.to_str() {
                let state = if item.ifa_flags & libc::IFF_UP as u32 == 0 {
                    "down"
                } else if item.ifa_flags & libc::IFF_RUNNING as u32 == 0 {
                    "dormant"
                } else {
                    "up"
                };
                states.insert(name.to_owned(), state.into());
            }
        }
        current = item.ifa_next;
    }
    states
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_inventory_has_no_bridge_or_tunnel_and_states_include_loopback() {
        let adapters = physical_adapters();
        assert!(
            !adapters.is_empty(),
            "native Mac must expose a network-capable interface"
        );
        for adapter in adapters {
            assert!(adapter.interface_name.is_some());
            assert!(!adapter.interface_name.unwrap().starts_with("bridge"));
            assert_eq!(adapter.source, "macos-system-configuration");
        }
        assert!(interface_states().contains_key("lo0"));
    }
}

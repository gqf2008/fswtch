//! FreeSWITCH NAT traversal (UPnP / NAT-PMP) — port mapping helpers.
//!
//! Init is normally driven by the FreeSWITCH core; these wrappers expose the mapping lifecycle
//! for modules that need to punch additional ports. Call [`is_initialized`] before
//! [`add_mapping`].
//!
//! `status()` is intentionally not wrapped: the upstream header documents its returned string as
//! "caller must free" without naming the deallocator, so freeing it safely cannot be guaranteed
//! from headers alone. Use [`type_str`] (a borrowed static string) for a safe status read.

use std::ffi::CStr;

use crate::{Pool, Result, status_to_result, sys};

/// IP protocol for a NAT mapping — `switch_nat_ip_proto_t`.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum NatIpProto {
    Udp,
    Tcp,
}

impl NatIpProto {
    fn raw(self) -> sys::switch_nat_ip_proto_t {
        match self {
            Self::Udp => sys::switch_nat_ip_proto_t_SWITCH_NAT_UDP,
            Self::Tcp => sys::switch_nat_ip_proto_t_SWITCH_NAT_TCP,
        }
    }
}

/// Initializes the NAT subsystem. `enable_mapping` toggles automatic port mapping.
///
/// Wraps `switch_nat_init`. Usually called by the core; exposed for modules that bootstrap a
/// standalone NAT context against `pool`.
pub fn init(pool: &Pool, enable_mapping: bool) {
    let mapping = bool_to_switch(enable_mapping);
    // SAFETY: `pool.as_ptr()` is a live APR pool; `mapping` is a valid switch_bool_t.
    unsafe { sys::switch_nat_init(pool.as_ptr(), mapping) };
}

/// Completes NAT subsystem init after other modules have loaded.
pub fn late_init() {
    // SAFETY: no arguments.
    unsafe { sys::switch_nat_late_init() };
}

/// `true` if the NAT subsystem has been initialized and mapping calls are usable.
pub fn is_initialized() -> bool {
    // SAFETY: no arguments; returns switch_bool_t.
    let result = unsafe { sys::switch_nat_is_initialized() };
    result != sys::switch_bool_t_SWITCH_FALSE
}

/// Maps internal `port` (UDP/TCP) to an external port via UPnP/PMP.
///
/// When `request_external` is `Some(desired)`, the desired external port is requested and the
/// actually-allocated port is returned (which may differ). Pass `None` to let the NAT device pick
/// and ignore the returned value. `sticky = true` marks the mapping persistent. Requires NAT to
/// be initialized ([`is_initialized`] returns `true`).
pub fn add_mapping(
    port: u16,
    proto: NatIpProto,
    request_external: Option<u16>,
    sticky: bool,
) -> Result<Option<u16>> {
    let mut external: sys::switch_port_t = request_external.unwrap_or(0);
    let external_ptr = if request_external.is_some() {
        &mut external as *mut _
    } else {
        std::ptr::null_mut()
    };
    let sticky = bool_to_switch(sticky);
    // SAFETY: `port`/`proto.raw()` are plain values; `external_ptr` is null or a valid out-param;
    // `sticky` is a valid switch_bool_t.
    let status = unsafe { sys::switch_nat_add_mapping(port, proto.raw(), external_ptr, sticky) };
    status_to_result(status)?;
    Ok(if request_external.is_some() {
        Some(external)
    } else {
        None
    })
}

/// Removes a previously added mapping.
pub fn del_mapping(port: u16, proto: NatIpProto) -> Result<()> {
    // SAFETY: plain value arguments.
    status_to_result(unsafe { sys::switch_nat_del_mapping(port, proto.raw()) })
}

/// The current active NAT mechanism (`"upnp"` / `"pmp"` / `"n/a"`), as a borrowed static C string.
///
/// Wraps `switch_nat_get_type`, which returns a const string that must not be freed.
pub fn type_str() -> Option<&'static CStr> {
    // SAFETY: returns null or a static const C string.
    let ptr = unsafe { sys::switch_nat_get_type() };
    if ptr.is_null() {
        None
    } else {
        // SAFETY: `ptr` is null or a static const C string.
        Some(unsafe { CStr::from_ptr(ptr) })
    }
}

/// Re-publishes all existing mappings (e.g. after a NAT gateway change).
pub fn republish() {
    // SAFETY: no arguments.
    unsafe { sys::switch_nat_republish() };
}

/// Re-initializes the NAT subsystem (full teardown + init).
pub fn reinit() {
    // SAFETY: no arguments.
    unsafe { sys::switch_nat_reinit() };
}

/// Shuts the NAT subsystem down.
pub fn shutdown() {
    // SAFETY: no arguments.
    unsafe { sys::switch_nat_shutdown() };
}

/// Toggles whether automatic port mapping is performed.
pub fn set_mapping(enable: bool) {
    // SAFETY: `enable` is a valid switch_bool_t.
    unsafe { sys::switch_nat_set_mapping(bool_to_switch(enable)) };
}

fn bool_to_switch(b: bool) -> sys::switch_bool_t {
    if b {
        sys::switch_bool_t_SWITCH_TRUE
    } else {
        sys::switch_bool_t_SWITCH_FALSE
    }
}

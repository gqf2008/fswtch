//! FreeSWITCH IP/ACL network lists — the programmatic face of `acl.conf.xml`.
//!
//! A [`NetworkList`] holds an ordered set of CIDR / host-mask rules with a default allow/deny,
//! and answers "is this IP permitted, and under which token?". The list borrows a [`Pool`] for
//! all storage (rules, tokens, the list itself): there is no destroy call, so the list's lifetime
//! is bounded by the pool and enforced by the borrow checker (`'pool`).

use std::ffi::CStr;
use std::net::Ipv4Addr;
use std::ptr::NonNull;

use crate::{GENERR, Pool, Result, SwitchError, cstring, status_to_result, sys};

/// The result of an ACL lookup: allowed or denied, plus the token of the matching rule when one
/// was hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AclVerdict<'a> {
    allowed: bool,
    /// The token label of the rule that decided this verdict, if any. Borrows list storage.
    token: Option<&'a CStr>,
}

impl<'a> AclVerdict<'a> {
    /// `true` when the IP matched an allow rule (or the list's default is allow and no deny hit).
    pub fn allowed(&self) -> bool {
        self.allowed
    }
    /// The token of the deciding rule, when the IP matched a labeled rule.
    pub fn token(&self) -> Option<&'a CStr> {
        self.token
    }
}

/// An IP/ACL network list, borrowing storage from a [`Pool`].
///
/// Rules added via [`add_cidr`](Self::add_cidr) / [`add_host`](Self::add_host) are matched in
/// longest-prefix order by [`validate_ipv4`](Self::validate_ipv4). The list lives as long as
/// `'pool`; there is no `destroy` — the pool reclaims everything on drop.
///
/// IPv6 lookup is not yet wrapped (the `ip_t` union's generated field path is unstable across
/// bindgen versions); use IPv4 for now.
pub struct NetworkList<'pool> {
    raw: NonNull<sys::switch_network_list_t>,
    _pool: &'pool Pool,
}

impl<'pool> NetworkList<'pool> {
    /// Creates a new list named `name` with the given `default_allow` policy.
    ///
    /// `default_allow = true` means IPs matching no rule are permitted; `false` means denied.
    /// The list borrows `pool` for all subsequent storage.
    pub fn new(name: impl AsRef<str>, default_allow: bool, pool: &'pool Pool) -> Result<Self> {
        let name = cstring(name)?;
        let default_type = if default_allow {
            sys::switch_bool_t_SWITCH_TRUE
        } else {
            sys::switch_bool_t_SWITCH_FALSE
        };
        let mut raw: *mut sys::switch_network_list_t = std::ptr::null_mut();
        // SAFETY: `&mut raw` is a valid out-param; `name` is a valid C string; `default_type` is a
        // valid switch_bool_t; `pool.as_ptr()` is a live APR pool the list will bind to.
        let status = unsafe {
            sys::switch_network_list_create(&mut raw, name.as_ptr(), default_type, pool.as_ptr())
        };
        status_to_result(status)?;
        let raw = NonNull::new(raw).ok_or(SwitchError(GENERR))?;
        Ok(Self { raw, _pool: pool })
    }

    /// Adds a CIDR rule (e.g. `"10.0.0.0/8"`). `allow = true` permits matching IPs. `token` labels
    /// the rule for later retrieval during [`validate_ipv4`](Self::validate_ipv4); pass `None` for
    /// an unlabeled rule. Interior NUL is rejected.
    pub fn add_cidr(&self, cidr: impl AsRef<str>, allow: bool, token: Option<&str>) -> Result<()> {
        let cidr = cstring(cidr)?;
        let token = match token {
            Some(t) => Some(cstring(t)?),
            None => None,
        };
        let ok = bool_to_switch(allow);
        let token_ptr = token.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
        // SAFETY: `self.raw` is a live list; `cidr`/`token` are valid C strings (or null) for the
        // call; `ok` is a valid switch_bool_t.
        let status = unsafe {
            sys::switch_network_list_add_cidr_token(self.raw.as_ptr(), cidr.as_ptr(), ok, token_ptr)
        };
        status_to_result(status)
    }

    /// Adds a host/mask rule (e.g. host `"10.0.0.1"`, mask `"255.0.0.0"`). Interior NUL is rejected.
    pub fn add_host(
        &self,
        host: impl AsRef<str>,
        mask: impl AsRef<str>,
        allow: bool,
    ) -> Result<()> {
        let host = cstring(host)?;
        let mask = cstring(mask)?;
        let ok = bool_to_switch(allow);
        // SAFETY: `self.raw` is a live list; `host`/`mask` are valid C strings; `ok` valid.
        let status = unsafe {
            sys::switch_network_list_add_host_mask(
                self.raw.as_ptr(),
                host.as_ptr(),
                mask.as_ptr(),
                ok,
            )
        };
        status_to_result(status)
    }

    /// Looks up `ip` against the list (longest-prefix match, default fallback). The returned
    /// [`AclVerdict`] borrows the matching rule's token for `self`'s lifetime.
    ///
    /// `ip` is converted to network byte order (`u32::from_be_bytes`) matching FreeSWITCH's
    /// internal `switch_test_subnet`, which compares against CIDRs parsed via `inet_pton`.
    pub fn validate_ipv4(&self, ip: Ipv4Addr) -> AclVerdict<'_> {
        let ip_u32 = u32::from_be_bytes(ip.octets());
        let mut token_ptr: *const std::ffi::c_char = std::ptr::null();
        // SAFETY: `self.raw` is a live list; `ip_u32` is a plain u32; `&mut token_ptr` is a valid
        // out-param (null when no rule matches). The returned switch_bool_t is 0/1.
        let allowed = unsafe {
            sys::switch_network_list_validate_ip_token(self.raw.as_ptr(), ip_u32, &mut token_ptr)
        } != sys::switch_bool_t_SWITCH_FALSE;
        // SAFETY: `token_ptr` is null or points at pool-backed storage tied to this list's
        // lifetime (`'pool`, which outlives `&self`); see `cstr_ptr_to_option`'s contract.
        let token = unsafe { cstr_ptr_to_option(token_ptr) };
        AclVerdict { allowed, token }
    }
}

fn bool_to_switch(b: bool) -> sys::switch_bool_t {
    if b {
        sys::switch_bool_t_SWITCH_TRUE
    } else {
        sys::switch_bool_t_SWITCH_FALSE
    }
}

/// Wraps a (possibly null) borrowed C-string pointer into `Option<&CStr>` without copying/freeing.
///
/// # Safety
///
/// When non-null, `ptr` must point at storage owned by FreeSWITCH that outlives the caller's use
/// of the returned reference (typically the list's pool). Null is treated as `None`.
unsafe fn cstr_ptr_to_option<'a>(ptr: *const std::ffi::c_char) -> Option<&'a CStr> {
    if ptr.is_null() {
        None
    } else {
        // SAFETY: caller guarantees `ptr` is null or a valid C string with pool-backed lifetime.
        Some(unsafe { CStr::from_ptr(ptr) })
    }
}

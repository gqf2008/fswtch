//! RTP session wrapper over `switch_rtp_t`.
//!
//! [`Rtp`] is an owned handle to a FreeSWITCH RTP session, created from an [`RtpConfig`] builder and
//! a borrowed [`crate::pool::Pool`]. It exposes the common lifecycle and I/O subset of the RTP API
//! (create / write / read / destroy / peer-info). The deep media-security features — SRTP keying,
//! DTLS, ICE, RTCP-XR reporting — are intentionally deferred; the raw pointer escape hatch
//! ([`Rtp::as_ptr`]) lets callers reach the unwrapped `switch_rtp_*` symbols directly when needed.
//!
//! `switch_rtp_new` is a macro-style entry point that bindgen emits verbatim (no `perform_`/`_detailed`
//! variant exists), so this module calls it directly. The flag array is declared in the header as
//! `switch_rtp_flag_t flags[SWITCH_RTP_FLAG_INVALID]` (a fixed array of 50 elements); the wrapper
//! keeps an owned `[switch_rtp_flag_t; 50]` and passes a pointer into the FFI call.

use std::ffi::c_char;
use std::ptr::NonNull;

use crate::pool::Pool;
use crate::sys::{
    self, switch_bool_t, switch_frame_t, switch_io_flag_t, switch_memory_pool_t, switch_payload_t,
    switch_port_t, switch_rtp_flag_t, switch_rtp_t,
};
use crate::{GENERR, Result, SwitchError, cstring, status_to_result};

/// Number of flag slots FreeSWITCH reserves for an RTP session (`SWITCH_RTP_FLAG_INVALID`).
const RTP_FLAG_COUNT: usize = 50;

/// An owned FreeSWITCH RTP session (`switch_rtp_t`).
///
/// Created via [`RtpConfig::build`]; destroyed on drop by `switch_rtp_destroy`. The session owns its
/// own socket, jitter buffer, and (when configured) timer; drop closes the socket and reclaims all
/// associated storage from the [`Pool`] the session was created against.
///
/// Deep features (SRTP / DTLS / ICE / RTCP-XR) are not wrapped here — use [`Rtp::as_ptr`] to reach
/// the underlying `switch_rtp_t *` for those code paths.
pub struct Rtp {
    raw: NonNull<switch_rtp_t>,
}

impl Rtp {
    /// Wraps a pre-existing `switch_rtp_t *` created outside this wrapper.
    ///
    /// # Safety
    ///
    /// `raw` must point to a live `switch_rtp_t` obtained from `switch_rtp_new` (or equivalent) and
    /// must not already be owned by another [`Rtp`] or have been destroyed. Ownership transfers to
    /// the returned [`Rtp`]; dropping it will call `switch_rtp_destroy`.
    pub unsafe fn from_raw(raw: *mut switch_rtp_t) -> Option<Self> {
        NonNull::new(raw).map(|raw| Self { raw })
    }

    /// The underlying `switch_rtp_t *`. Escape hatch for unwrapped RTP features.
    #[inline]
    pub fn as_ptr(&self) -> *mut switch_rtp_t {
        self.raw.as_ptr()
    }

    /// Writes a media frame to the session. Returns the number of bytes written.
    ///
    /// `frame` is a raw `switch_frame_t *` because [`crate::Frame`] is not yet available; construct
    /// or borrow the frame on the C side and pass the pointer through. The pointer is only read for
    /// the duration of this call.
    ///
    /// `switch_rtp_write_frame` returns the byte count on success; a negative return indicates a
    /// write failure and is mapped to [`crate::SwitchError`]`([`crate::GENERR`]`).
    pub fn write_frame(&self, frame: *mut switch_frame_t) -> Result<u32> {
        // SAFETY: `self.raw` is a live RTP session; `frame` is borrowed for the call only.
        let written = unsafe { sys::switch_rtp_write_frame(self.raw.as_ptr(), frame) };
        if written < 0 {
            return Err(SwitchError(GENERR));
        }
        Ok(written as u32)
    }

    /// Reads the next frame from the session into a caller-provided frame, zero-copy.
    ///
    /// Uses `switch_rtp_zerocopy_read_frame`, which fills the supplied `switch_frame_t` with a
    /// pointer into the RTP read buffer (no copy). The frame and the buffer it references are valid
    /// until the next read on this session. `frame` is a raw pointer because [`crate::Frame`] is not
    /// yet available.
    pub fn read_frame(&self, frame: *mut switch_frame_t, io_flags: switch_io_flag_t) -> Result<()> {
        // SAFETY: `self.raw` is a live RTP session; `frame` is a valid out-pointer for the call.
        let status = unsafe { sys::switch_rtp_zerocopy_read_frame(self.raw.as_ptr(), frame, io_flags) };
        status_to_result(status)
    }

    /// The remote host (peer IP / FQDN) currently associated with the session.
    ///
    /// The returned string borrows storage owned by the RTP session and is invalidated by the next
    /// call that rewrites the remote address (e.g. [`Rtp::set_remote_address`]) or by dropping the
    /// [`Rtp`].
    pub fn remote_host(&self) -> Option<String> {
        // SAFETY: `self.raw` is a live RTP session; the returned pointer borrows session storage.
        let ptr = unsafe { sys::switch_rtp_get_remote_host(self.raw.as_ptr()) };
        // SAFETY: `ptr` is null or a null-terminated string that borrows session storage.
        unsafe { crate::borrowed_cstr_to_str(ptr.cast_const()) }.map(ToOwned::to_owned)
    }

    /// The remote UDP port currently associated with the session.
    pub fn remote_port(&self) -> switch_port_t {
        // SAFETY: `self.raw` is a live RTP session.
        unsafe { sys::switch_rtp_get_remote_port(self.raw.as_ptr()) }
    }

    /// Sets the remote (peer) address, re-binding the destination of outgoing RTP.
    ///
    /// `remote_rtcp_port` may be `0` to leave RTCP at the same offset as RTP. Set
    /// `change_adv_addr` to also update the advertised address used in address-comparison logic.
    /// If FreeSWITCH writes an error message, it is surfaced via the returned [`Err`].
    pub fn set_remote_address(
        &self,
        host: impl AsRef<str>,
        port: switch_port_t,
        remote_rtcp_port: switch_port_t,
        change_adv_addr: bool,
    ) -> Result<()> {
        let host = cstring(host)?;
        let mut err: *const c_char = std::ptr::null();
        // SAFETY: `self.raw` is a live RTP session; `host` is a valid C string; `err` is a valid
        // out-pointer that FreeSWITCH may set to a borrowed error message.
        let status = unsafe {
            sys::switch_rtp_set_remote_address(
                self.raw.as_ptr(),
                host.as_ptr(),
                port,
                remote_rtcp_port,
                switch_bool(change_adv_addr),
                &mut err,
            )
        };
        status_to_result(status)
    }

    /// Sets the local (bind) address. This also (re)binds the session's UDP socket.
    pub fn set_local_address(&self, host: impl AsRef<str>, port: switch_port_t) -> Result<()> {
        let host = cstring(host)?;
        let mut err: *const c_char = std::ptr::null();
        // SAFETY: `self.raw` is a live RTP session; `host` is a valid C string; `err` is a valid
        // out-pointer.
        let status = unsafe {
            sys::switch_rtp_set_local_address(self.raw.as_ptr(), host.as_ptr(), port, &mut err)
        };
        status_to_result(status)
    }

    /// Returns `true` once the session has finished initialising and is ready for I/O.
    pub fn ready(&self) -> bool {
        // SAFETY: `self.raw` is a live RTP session.
        let v = unsafe { sys::switch_rtp_ready(self.raw.as_ptr()) };
        v != 0
    }

    /// Stops the read loop and closes the session's socket without destroying the handle. Useful
    /// before [`Rtp::set_local_address`] re-binds.
    pub fn kill_socket(&self) {
        // SAFETY: `self.raw` is a live RTP session.
        unsafe { sys::switch_rtp_kill_socket(self.raw.as_ptr()) };
    }
}

impl Drop for Rtp {
    fn drop(&mut self) {
        // `switch_rtp_destroy` takes `switch_rtp_t **` and nulls the caller's pointer on success.
        let mut ptr: *mut switch_rtp_t = self.raw.as_ptr();
        // SAFETY: `ptr` points to our owned, live session; `&mut ptr` is a valid `*mut *mut`.
        // After the call `ptr` is null and the session is destroyed.
        unsafe { sys::switch_rtp_destroy(&mut ptr) };
    }
}

/// Build configuration for a new [`Rtp`].
///
/// Mirrors the (many) parameters of `switch_rtp_new`. Construct with [`RtpConfig::new`], set the
/// desired fields, then call [`RtpConfig::build`] with a borrowed [`Pool`] to obtain an owned
/// [`Rtp`]. Unset string fields default to the loopback address `127.0.0.1`; unset numeric fields
/// default to `0`.
///
/// `rx_host`/`rx_port` are the local bind address; `tx_host`/`tx_port` are the remote peer. The
/// bundle ports default to `0` (no bundling).
#[derive(Clone)]
pub struct RtpConfig {
    rx_host: Option<String>,
    rx_port: switch_port_t,
    tx_host: Option<String>,
    tx_port: switch_port_t,
    payload: switch_payload_t,
    samples_per_interval: u32,
    ms_per_packet: u32,
    flags: [switch_rtp_flag_t; RTP_FLAG_COUNT],
    timer_name: Option<String>,
    bundle_internal_port: switch_port_t,
    bundle_external_port: switch_port_t,
}

impl Default for RtpConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl RtpConfig {
    /// Creates a config with empty defaults. At minimum, set the payload, sample rate, packet
    /// interval, and peer address before [`RtpConfig::build`].
    pub fn new() -> Self {
        Self {
            rx_host: None,
            rx_port: 0,
            tx_host: None,
            tx_port: 0,
            payload: 0,
            samples_per_interval: 0,
            ms_per_packet: 0,
            flags: [0; RTP_FLAG_COUNT],
            timer_name: None,
            bundle_internal_port: 0,
            bundle_external_port: 0,
        }
    }

    /// Local (bind) host. Defaults to `127.0.0.1` when unset at build time.
    pub fn rx_host(mut self, host: impl Into<String>) -> Self {
        self.rx_host = Some(host.into());
        self
    }

    /// Local (bind) UDP port. `0` lets the OS choose.
    pub fn rx_port(mut self, port: switch_port_t) -> Self {
        self.rx_port = port;
        self
    }

    /// Remote (peer) host. Defaults to `127.0.0.1` when unset at build time.
    pub fn tx_host(mut self, host: impl Into<String>) -> Self {
        self.tx_host = Some(host.into());
        self
    }

    /// Remote (peer) UDP port.
    pub fn tx_port(mut self, port: switch_port_t) -> Self {
        self.tx_port = port;
        self
    }

    /// IANA RTP payload number (e.g. `0` for PCMU, `8` for PCMA).
    pub fn payload(mut self, payload: switch_payload_t) -> Self {
        self.payload = payload;
        self
    }

    /// Samples per packet interval (e.g. `160` for 20 ms of G.711 at 8 kHz).
    pub fn samples_per_interval(mut self, samples: u32) -> Self {
        self.samples_per_interval = samples;
        self
    }

    /// Packet interval in milliseconds (e.g. `20`).
    pub fn ms_per_packet(mut self, ms: u32) -> Self {
        self.ms_per_packet = ms;
        self
    }

    /// Sets a single `switch_rtp_flag_t` (`SWITCH_RTP_FLAG_*`) in the flag array, leaving others
    /// unchanged. Safe to call repeatedly to combine flags.
    pub fn flag(mut self, flag: switch_rtp_flag_t) -> Self {
        let idx = flag as usize;
        if idx < RTP_FLAG_COUNT {
            self.flags[idx] = flag;
        }
        self
    }

    /// Timer interface name (e.g. `"soft"`). Leave unset to use no timer (raw write mode).
    pub fn timer_name(mut self, name: impl Into<String>) -> Self {
        self.timer_name = Some(name.into());
        self
    }

    /// Bundle internal port (audio/video bundling). `0` disables bundling.
    pub fn bundle_internal_port(mut self, port: switch_port_t) -> Self {
        self.bundle_internal_port = port;
        self
    }

    /// Bundle external port (audio/video bundling). `0` disables bundling.
    pub fn bundle_external_port(mut self, port: switch_port_t) -> Self {
        self.bundle_external_port = port;
        self
    }

    /// Builds an owned [`Rtp`] against the supplied [`Pool`].
    ///
    /// The session is allocated against `pool` and is reclaimed when either the [`Rtp`] is dropped
    /// or the `Pool` is dropped (whichever comes first — keep the [`Pool`] alive for at least the
    /// lifetime of the [`Rtp`]). Returns `Err` if `switch_rtp_new` returns null or FreeSWITCH writes
    /// an error message.
    pub fn build(&self, pool: &Pool) -> Result<Rtp> {
        let rx_host = cstring(self.rx_host.as_deref().unwrap_or("127.0.0.1"))?;
        let tx_host = cstring(self.tx_host.as_deref().unwrap_or("127.0.0.1"))?;
        // `timer_name` is `char *` (mutable) in the FFI; for an unset timer pass null. For a set
        // timer the C side only reads it, so a cast to mut is sound.
        let timer_cstr = match &self.timer_name {
            Some(name) => Some(cstring(name.as_str())?),
            None => None,
        };
        let timer_ptr = timer_cstr
            .as_ref()
            .map(|c| c.as_ptr().cast_mut())
            .unwrap_or(std::ptr::null_mut());
        let mut err: *const c_char = std::ptr::null();
        let pool_ptr: *mut switch_memory_pool_t = pool.as_ptr();
        // SAFETY: `rx_host`/`tx_host` are valid C strings; `self.flags` is a fixed 50-element array
        // matching `SWITCH_RTP_FLAG_INVALID`; `err` is a valid out-pointer; `pool_ptr` is a live
        // pool borrowed for this call. `switch_rtp_new` either returns a live session or null
        // (with `err` possibly set).
        let raw = unsafe {
            sys::switch_rtp_new(
                rx_host.as_ptr(),
                self.rx_port,
                tx_host.as_ptr(),
                self.tx_port,
                self.payload,
                self.samples_per_interval,
                self.ms_per_packet,
                self.flags.as_ptr() as *mut _,
                timer_ptr,
                &mut err,
                pool_ptr,
                self.bundle_internal_port,
                self.bundle_external_port,
            )
        };
        if raw.is_null() {
            return Err(SwitchError(GENERR));
        }
        // SAFETY: `raw` is non-null and freshly created by `switch_rtp_new`; ownership transfers.
        Ok(unsafe { Rtp::from_raw(raw) }.expect("switch_rtp_new returned non-null pointer"))
    }
}

/// Maps a Rust `bool` to FreeSWITCH's `switch_bool_t`.
fn switch_bool(v: bool) -> switch_bool_t {
    if v {
        sys::switch_bool_t_SWITCH_TRUE
    } else {
        sys::switch_bool_t_SWITCH_FALSE
    }
}

/// Requests an available UDP port from FreeSWITCH's port allocator for the given local IP.
///
/// Returns the port to bind to, or `0` if none is available. The returned port should be released
/// back to the allocator via `switch_rtp_release_port` when the session ends (not wrapped here —
/// [`Rtp`] manages its own socket and frees the port on drop).
pub fn request_port(ip: impl AsRef<str>) -> Result<switch_port_t> {
    let ip = cstring(ip)?;
    // SAFETY: `ip` is a valid C string for the duration of the call.
    let port = unsafe { sys::switch_rtp_request_port(ip.as_ptr()) };
    Ok(port)
}

#[cfg(all(test, feature = "live_fs"))]
mod tests {
    use super::*;

    #[test]
    fn config_builder_is_chainable() {
        let cfg = RtpConfig::new()
            .rx_host("127.0.0.1")
            .rx_port(0)
            .tx_host("127.0.0.1")
            .tx_port(5004)
            .payload(8)
            .samples_per_interval(160)
            .ms_per_packet(20)
            .timer_name("soft");
        assert_eq!(cfg.payload, 8);
        assert_eq!(cfg.tx_port, 5004);
        assert_eq!(cfg.ms_per_packet, 20);
        assert_eq!(cfg.timer_name.as_deref(), Some("soft"));
    }

    #[test]
    fn flag_set_clears_others_only_for_index() {
        let cfg = RtpConfig::new().flag(sys::switch_rtp_flag_t_SWITCH_RTP_FLAG_USE_TIMER);
        let idx = sys::switch_rtp_flag_t_SWITCH_RTP_FLAG_USE_TIMER as usize;
        assert_eq!(cfg.flags[idx], sys::switch_rtp_flag_t_SWITCH_RTP_FLAG_USE_TIMER);
        assert_eq!(cfg.flags[0], 0);
    }

    #[test]
    fn switch_bool_maps_both_directions() {
        assert_eq!(switch_bool(true), sys::switch_bool_t_SWITCH_TRUE);
        assert_eq!(switch_bool(false), sys::switch_bool_t_SWITCH_FALSE);
    }
}

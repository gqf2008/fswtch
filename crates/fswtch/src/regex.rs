//! PCRE2-backed regular expressions.
//!
//! Wraps the helpers in `switch_regex.h`. Two layers are exposed:
//!
//! - [`Regex`] owns a *compiled* pattern (produced by `switch_regex_compile`) plus the original
//!   pattern text. The compiled code is freed on `Drop` via `switch_regex_free`. Building once and
//!   matching many subjects lets callers reuse the pattern; note that the underlying FreeSWITCH
//!   match helpers recompile internally (see below), so reuse here is mostly about validation and
//!   the `as_ptr` escape hatch.
//! - [`RegexMatch`] owns the *match data* produced by matching a [`Regex`] against a single
//!   subject. It exposes captured groups and substitution, and frees the underlying match data on
//!   `Drop` via `switch_regex_match_free`.
//!
//! For one-off boolean tests where neither compilation reuse nor capture extraction is needed, the
//! free functions [`is_match`] and [`is_match_partial`] call `switch_regex_match` /
//! `switch_regex_match_partial` directly.
//!
//! `switch_capture_regex` is left unwrapped — see [`CaptureCallback`].
//!
//! # Note on the underlying API
//!
//! `switch_regex.h` does not expose a "match a precompiled regex against a subject and receive
//! match data" function. The capture-producing helper, `switch_regex_perform`, takes the pattern as
//! a string and compiles it itself on every call (it also returns a freshly compiled `re` for the
//! caller to free, which this wrapper discards). `Regex::matches` therefore stores the pattern text
//! and routes through `switch_regex_perform`; `self.raw` (the validated compiled code) is kept for
//! `as_ptr` and freed on `Drop`.

use std::ffi::CString;
use std::ptr::NonNull;

use crate::{Result, SwitchError, sys};
use crate::{GENERR, SUCCESS};

/// A compiled PCRE2 regular expression.
///
/// Build once with [`Regex::compile`] and match many subjects with [`Regex::matches`]. The compiled
/// code is released when this value is dropped.
///
/// `options` is the PCRE2 options bitset (e.g. `PCRE2_CASELESS`, `PCRE2_MULTILINE`). FreeSWITCH
/// does not re-export the `PCRE2_*` constants through `switch_regex.h`, so pass the raw integer the
/// PCRE2 library expects.
pub struct Regex {
    raw: Option<NonNull<sys::switch_regex_t>>,
    pattern: CString,
}

impl Regex {
    /// Compiles `pattern` with the given PCRE2 `options` bitset.
    ///
    /// `options` follows the PCRE2 convention (`0` for default, case-sensitive matching). Returns
    /// an error if the pattern fails to compile. The compiled code is retained for the lifetime of
    /// this value and freed on `Drop`; the pattern text is also stored so that subsequent matches
    /// can route through `switch_regex_perform`.
    pub fn compile(pattern: &str, options: u32) -> Result<Self> {
        let pat = crate::cstring(pattern)?;
        let mut errorcode: std::os::raw::c_int = 0;
        let mut erroroffset: std::os::raw::c_uint = 0;
        // SAFETY: `pat` is a valid null-terminated C string for the duration of the call; the two
        // out-pointers are valid locals and a null compile context is permitted.
        let raw = unsafe {
            sys::switch_regex_compile(
                pat.as_ptr(),
                options as std::os::raw::c_int,
                &mut errorcode,
                &mut erroroffset,
                std::ptr::null_mut(),
            )
        };
        let raw = NonNull::new(raw).ok_or(SwitchError(GENERR))?;
        Ok(Self { raw: Some(raw), pattern: pat })
    }

    /// Wraps an already-compiled FreeSWITCH regex pointer together with its source pattern text.
    ///
    /// # Safety
    ///
    /// `raw` must point to a `switch_regex_t` produced by `switch_regex_compile` (or
    /// `switch_regex_perform`) that the caller transfers ownership of, and which has not yet been
    /// freed. `pattern` must be the exact pattern text that produced `raw`.
    pub unsafe fn from_raw(raw: *mut sys::switch_regex_t, pattern: CString) -> Option<Self> {
        NonNull::new(raw).map(|raw| Self { raw: Some(raw), pattern })
    }

    /// The raw compiled-regex pointer, for direct FFI use.
    #[inline]
    pub fn as_ptr(&self) -> *mut sys::switch_regex_t {
        self.raw.map_or(std::ptr::null_mut(), NonNull::as_ptr)
    }

    /// The pattern text this regex was compiled from.
    #[inline]
    pub fn pattern(&self) -> &str {
        // SAFETY: `self.pattern` was constructed from a `&str` and contains no interior NULs.
        self.pattern
            .to_str()
            .unwrap_or("")
    }

    /// Matches `subject` against this regex.
    ///
    /// On success returns a [`RegexMatch`] that owns the match data and the pattern; use it to
    /// extract captured groups or to run a substitution. Returns `Ok(None)` when the subject does
    /// not match (no error).
    ///
    /// Drives `switch_regex_perform`, which returns a *match count*: a positive value on a match
    /// (one per capture pair, so `1` for a bare match, `2` for one group, ...) and `0` on no match.
    /// The freshly compiled regex it also yields is freed immediately; only the match data is kept.
    pub fn matches(&self, subject: &str) -> Result<Option<RegexMatch>> {
        let field = crate::cstring(subject)?;
        let mut new_re: *mut sys::switch_regex_t = std::ptr::null_mut();
        let mut new_match_data: *mut sys::switch_regex_match_t = std::ptr::null_mut();
        // SAFETY: `field` and `self.pattern` are valid C strings for the call; both out-pointers are
        // valid locals.
        let count = unsafe {
            sys::switch_regex_perform(
                field.as_ptr(),
                self.pattern.as_ptr(),
                &mut new_re,
                &mut new_match_data,
            )
        };
        // `switch_regex_perform` always compiles a fresh `re` and stores it in `*new_re`; it is not
        // the same object as `self.raw` and must be freed here.
        if !new_re.is_null() {
            // SAFETY: `new_re` was produced by `switch_regex_perform` and is now unreferenced.
            unsafe { sys::switch_regex_free(new_re.cast()) };
        }
        if count <= 0 {
            // No match. `switch_regex_perform` allocates `new_match_data` only on a successful
            // match (consistent with FreeSWITCH's `switch_regex_match_safe_free` macro, which
            // null-checks before freeing), so it is left NULL here and needs no freeing. We do
            // NOT free defensively: without the vendored `.c` we cannot prove perform frees it
            // internally, and a stray free on an already-freed pointer would be a double-free,
            // whereas a leak on the no-match path is merely a minor one.
            return Ok(None);
        }
        if new_match_data.is_null() {
            return Err(SwitchError(GENERR));
        }
        // SAFETY: `new_match_data` is a freshly allocated, non-null match-data pointer.
        let md = unsafe { NonNull::new_unchecked(new_match_data) };
        Ok(Some(RegexMatch {
            raw: Some(md),
            group_count: count,
        }))
    }

    /// Returns `true` when `subject` matches this regex. A convenience over [`Regex::matches`]
    /// that discards the match data.
    pub fn is_match(&self, subject: &str) -> bool {
        self.matches(subject).is_ok_and(|m| m.is_some())
    }
}

impl Drop for Regex {
    fn drop(&mut self) {
        if let Some(raw) = self.raw.take() {
            // SAFETY: `raw` owns a compiled regex that has not yet been freed; `switch_regex_free`
            // accepts `void *` and tolerates the cast.
            unsafe { sys::switch_regex_free(raw.as_ptr().cast()) };
        }
    }
}

/// The result of matching a [`Regex`] against a single subject.
///
/// Owns the PCRE2 match data (freed on `Drop` via `switch_regex_match_free`). Extract captured
/// groups with [`RegexMatch::capture`] or run a substitution template with [`RegexMatch::substitute`].
pub struct RegexMatch {
    raw: Option<NonNull<sys::switch_regex_match_t>>,
    group_count: std::os::raw::c_int,
}

impl RegexMatch {
    /// Wraps an existing FreeSWITCH match-data pointer.
    ///
    /// # Safety
    ///
    /// `raw` must point to a `switch_regex_match_t` produced by `switch_regex_perform` (or PCRE2)
    /// that the caller transfers ownership of, and which has not yet been freed. `group_count`
    /// must be the value `switch_regex_perform` returned for that match.
    pub unsafe fn from_raw(
        raw: *mut sys::switch_regex_match_t,
        group_count: i32,
    ) -> Option<Self> {
        NonNull::new(raw).map(|raw| Self {
            raw: Some(raw),
            group_count,
        })
    }

    /// The raw match-data pointer, for direct FFI use.
    #[inline]
    pub fn as_ptr(&self) -> *mut sys::switch_regex_match_t {
        self.raw.map_or(std::ptr::null_mut(), NonNull::as_ptr)
    }

    /// The number of capture pairs reported by the matcher.
    ///
    /// This is the value `switch_regex_perform` returned: `1` for a bare match with no groups,
    /// `1 + n` for `n` capturing groups. A capture group at `index` is valid only when
    /// `index < group_count()`.
    #[inline]
    pub fn group_count(&self) -> u32 {
        if self.group_count < 0 {
            0
        } else {
            self.group_count as u32
        }
    }

    /// Extracts the captured substring at `index`.
    ///
    /// `0` is the whole match; `1..n` are the capture groups in pattern order. Returns `Ok(None)`
    /// when the group did not participate in the match (or `index` is out of range), and `Err` if
    /// the underlying copy fails.
    pub fn capture(&self, index: u32) -> Result<Option<String>> {
        if index >= self.group_count() {
            return Ok(None);
        }
        // `switch_regex_copy_substring` forwards to `pcre2_substring_copy_bynumber`. Per its
        // contract: `*size` is the buffer capacity on input and the substring length (excluding the
        // terminator) on success; when the buffer is too small it returns a negative error and
        // sets `*size` to the required length. PCRE2_ERROR_NOSUBSTRING is returned when the group
        // exists in the pattern but did not participate, with `*size` left at 0.
        //
        // Start with a modest stack-allocated buffer and grow once if PCRE2 asks for more, avoiding
        // any reliance on an undocumented NULL-buffer probe.
        let mut buf: [u8; 256] = [0u8; 256];
        let mut size: usize = buf.len();
        // SAFETY: `self.raw` is a live match-data pointer; `buf` has `size` bytes and remains valid
        // for the call; `size` is a valid inout slot.
        let rc = unsafe {
            sys::switch_regex_copy_substring(
                self.as_ptr(),
                index as std::os::raw::c_int,
                buf.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if rc < 0 && size == 0 {
            // Group did not participate in this match.
            return Ok(None);
        }
        if rc < 0 {
            // Buffer too small: `size` now holds the required length. Allocate exactly and retry.
            let need = size + 1; // room for the terminator
            let mut big = vec![0u8; need];
            let mut size2: usize = need;
            // SAFETY: `big` has `need` bytes and remains valid for the call; `size2` is a valid
            // inout slot.
            let rc = unsafe {
                sys::switch_regex_copy_substring(
                    self.as_ptr(),
                    index as std::os::raw::c_int,
                    big.as_mut_ptr().cast(),
                    &mut size2,
                )
            };
            if rc != 0 {
                return Err(SwitchError(GENERR));
            }
            return std::str::from_utf8(&big[..size2])
                .map(|s| s.to_owned())
                .map(Some)
                .map_err(|_| SwitchError(GENERR));
        }
        // rc == 0: success, `size` is the substring length (excluding terminator).
        std::str::from_utf8(&buf[..size])
            .map(|s| s.to_owned())
            .map(Some)
            .map_err(|_| SwitchError(GENERR))
    }

    /// Substitutes captured groups into `template` and returns the result.
    ///
    /// `template` follows FreeSWITCH's `switch_perform_substitution` convention: `$1`..`$9` and
    /// `${1}`..`${256}` reference capture groups. The output is null-terminated by the C helper and
    /// at most `len - 1` bytes are written.
    pub fn substitute(&self, template: &str) -> Result<String> {
        let tmpl = crate::cstring(template)?;
        // FreeSWITCH callers typically allocate `len(data) + len(field) * 3 + 1`. We do not have
        // the field length here, so use a generous multiple of the template length with a floor.
        let cap = tmpl.as_bytes().len().saturating_mul(8).max(256) + 1;
        let mut out = vec![0u8; cap];
        // SAFETY: `tmpl` is a valid C string; `out` has `cap` writable bytes and remains valid for
        // the call. `switch_perform_substitution` writes at most `len - 1` bytes plus a NUL.
        unsafe {
            sys::switch_perform_substitution(
                self.as_ptr(),
                tmpl.as_ptr(),
                out.as_mut_ptr().cast(),
                cap as sys::switch_size_t,
            );
        }
        // The result is a null-terminated C string inside `out`.
        let nul = out.iter().position(|&b| b == 0).unwrap_or(out.len());
        std::str::from_utf8(&out[..nul])
            .map(|s| s.to_owned())
            .map_err(|_| SwitchError(GENERR))
    }
}

impl Drop for RegexMatch {
    fn drop(&mut self) {
        if let Some(raw) = self.raw.take() {
            // SAFETY: `raw` owns a match-data object that has not yet been freed;
            // `switch_regex_match_free` accepts `void *` and tolerates the cast.
            unsafe { sys::switch_regex_match_free(raw.as_ptr().cast()) };
        }
    }
}

/// A one-shot boolean regex test: returns `true` when `subject` matches `expression`.
///
/// Compiles and matches in a single call (`switch_regex_match`) with no captured groups and no
/// retained state. Prefer [`Regex::compile`] + [`Regex::is_match`] when matching many subjects
/// against the same pattern.
pub fn is_match(subject: &str, expression: &str) -> Result<bool> {
    let s = crate::cstring(subject)?;
    let e = crate::cstring(expression)?;
    // SAFETY: both pointers are valid null-terminated C strings for the duration of the call.
    let status = unsafe { sys::switch_regex_match(s.as_ptr(), e.as_ptr()) };
    Ok(status == SUCCESS)
}

/// Like [`is_match`] but also reports whether the match was partial.
///
/// Returns `Ok(Some(true))` for a full match, `Ok(Some(false))` for a partial match, and
/// `Ok(None)` when `subject` does not match `expression` at all.
pub fn is_match_partial(subject: &str, expression: &str) -> Result<Option<bool>> {
    let s = crate::cstring(subject)?;
    let e = crate::cstring(expression)?;
    let mut partial: std::os::raw::c_int = 0;
    // SAFETY: both pointers are valid C strings; `partial` is a valid local for the out-value.
    let status = unsafe { sys::switch_regex_match_partial(s.as_ptr(), e.as_ptr(), &mut partial) };
    if status == SUCCESS {
        Ok(Some(partial == 0))
    } else {
        Ok(None)
    }
}

/// Marker type reserved for a future safe adapter over `switch_capture_regex`.
///
/// `switch_capture_regex` takes a `switch_cap_callback_t` (a raw `extern "C" fn`) and invokes it
/// once per captured group. Exposing it safely would require a trampoline that boxes a Rust
/// closure and recovers it from `user_data`, plus `catch_unwind` panic isolation — the same
/// pattern used by [`crate::media`] media-bug handlers. That adapter is intentionally out of scope
/// for this module; callers needing capture iteration should use [`RegexMatch::capture`] with
/// explicit group indices instead.
pub enum CaptureCallback {}

#[cfg(all(test, feature = "live_fs"))]
mod tests {
    use super::*;

    #[test]
    fn compile_and_match() {
        // These tests run only when linked against a real FreeSWITCH build; if the FFI symbols are
        // not present the process will fail to start, which is the intended gate.
        let re = Regex::compile(r"^\d{3}-\d{4}$", 0).expect("compile");
        assert!(re.is_match("555-1234"));
        assert!(!re.is_match("abc"));
    }

    #[test]
    fn capture_groups() {
        let re = Regex::compile(r"^(\w+)-(\w+)$", 0).expect("compile");
        let m = re.matches("foo-bar").expect("match").expect("present");
        assert_eq!(m.capture(0).unwrap(), Some("foo-bar".to_owned()));
        assert_eq!(m.capture(1).unwrap(), Some("foo".to_owned()));
        assert_eq!(m.capture(2).unwrap(), Some("bar".to_owned()));
        assert_eq!(m.capture(3).unwrap(), None);
    }

    #[test]
    fn free_fn_one_shot() {
        assert!(is_match("hello", r"^h.llo$").unwrap());
        assert!(!is_match("world", r"^h.llo$").unwrap());
    }
}

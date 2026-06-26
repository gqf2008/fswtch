//! Common string-utility helpers from `switch_utils.h`.
//!
//! These wrap the handful of FreeSWITCH string functions that take a caller-provided buffer and
//! return a canonical, escaped, or encoded form. Each public function owns its output buffer,
//! calls the FFI, and copies the NUL-terminated result into an owned [`String`], so no raw
//! pointers or pools leak into the public API.
//!
//! Note: these functions are *not* the macro-prefixed `perform_`/`_strdup` family — bindgen emits
//! them verbatim under their real names.

use std::ffi::c_char;

use crate::command::borrowed_cstr_to_string;
use crate::{Result, SwitchError, cstring, sys, GENERR};

/// Worst-case growth factor for the escape / URL-encode routines: each source byte may expand to
/// at most three bytes (`%XX`), plus a trailing NUL.
const EXPAND_FACTOR: usize = 3;

/// Sizes an output buffer large enough to hold `len` input bytes under any expansion, plus a NUL.
fn out_capacity(len: usize) -> usize {
    len.saturating_mul(EXPAND_FACTOR)
        .saturating_add(1)
        .max(1)
}

/// Escapes a string the way FreeSWITCH does internally (URL-ish escaping via
/// `switch_escape_string`).
///
/// `switch_escape_string` writes the escaped form into a caller-supplied buffer of `outlen` bytes
/// and returns that same buffer. This function sizes the buffer to at most 3× the input length
/// (each byte can expand to `%XX`) plus a NUL, calls the FFI, and copies the NUL-terminated result
/// into an owned [`String`].
pub fn escape_string(s: &str) -> Result<String> {
    let input = cstring(s)?;
    let cap = out_capacity(s.len());
    let mut buf = vec![0u8 as c_char; cap];
    // SAFETY: `input` is a valid NUL-terminated C string; `buf` points to `cap` writable bytes
    // (>= 3*len + 1), sufficient for any expansion plus the NUL terminator written by the FFI.
    let written = unsafe {
        sys::switch_escape_string(
            input.as_ptr(),
            buf.as_mut_ptr(),
            cap as sys::switch_size_t,
        )
    };
    // `switch_escape_string` returns the `out` buffer on success; a null return indicates failure.
    if written.is_null() {
        return Err(SwitchError(GENERR));
    }
    // SAFETY: `written` is `buf` (non-null, guaranteed by the null check above) and now holds a
    // valid NUL-terminated C string produced by the FFI.
    Ok(borrowed_cstr_to_string(written).unwrap_or_default())
}

/// Percent-encodes a string for use in a URL (via `switch_url_encode`).
///
/// `switch_url_encode` writes the percent-encoded form into a caller-supplied buffer of `len`
/// bytes and returns that buffer. Each input byte can expand to `%XX`, so the buffer is sized to
/// 3× the input length plus a NUL.
pub fn url_encode(s: &str) -> Result<String> {
    let input = cstring(s)?;
    let cap = out_capacity(s.len());
    let mut buf = vec![0u8 as c_char; cap];
    // SAFETY: `input` is a valid NUL-terminated C string; `buf` points to `cap` writable bytes
    // (>= 3*len + 1), sufficient for the percent-encoded expansion plus the NUL terminator.
    let written = unsafe { sys::switch_url_encode(input.as_ptr(), buf.as_mut_ptr(), cap) };
    if written.is_null() {
        return Err(SwitchError(GENERR));
    }
    // SAFETY: `written` is `buf` and now holds a valid NUL-terminated C string.
    Ok(borrowed_cstr_to_string(written).unwrap_or_default())
}

/// Canonicalizes a numeric *string* using FreeSWITCH's `switch_format_number`.
///
/// `switch_format_number` accepts a string representation of a number (e.g. `"1001"`) and returns
/// a canonicalized form. This overload formats the integer `n` in decimal, passes it through the
/// FFI, and copies the result into an owned [`String`].
///
/// The returned pointer from `switch_format_number` has no `_strdup`/`_pool` sibling in the
/// generated bindings, so its result is treated as borrowed storage (matching FreeSWITCH's
/// static-buffer convention for pool-less accessors elsewhere in the crate) and copied out.
pub fn format_number(n: u64) -> Result<String> {
    let digits = cstring(n.to_string())?;
    // SAFETY: `digits` is a valid NUL-terminated C string holding the decimal form of `n`.
    let formatted = unsafe { sys::switch_format_number(digits.as_ptr()) };
    if formatted.is_null() {
        return Err(SwitchError(GENERR));
    }
    // SAFETY: `formatted` is non-null and points to a NUL-terminated C string produced by the FFI.
    Ok(borrowed_cstr_to_string(formatted).unwrap_or_default())
}

/// Finds the index of the matching closing bracket for the first unmatched `open` in `s`.
///
/// Wraps `switch_find_end_paren(s, open, close)`, which returns a pointer into `s` at the position
/// of the matching `close` character (past the `open`), or `NULL` when no balanced pair exists.
/// This function returns the byte offset of that `close` character, or `None` if there is no match.
///
/// Because the C function returns a pointer into the *same* `s`, the offset is computed as the
/// difference between the returned pointer and `s`'s base — no buffer copy is involved.
pub fn find_end_paren(s: &str, open: char, close: char) -> Option<usize> {
    // `switch_find_end_paren` takes plain `char` arguments; non-ASCII `open`/`close` cannot be
    // represented and are rejected up front.
    let open_b = u8::try_from(open).ok()?;
    let close_b = u8::try_from(close).ok()?;
    let input = cstring(s).ok()?;
    let base = input.as_ptr();
    // SAFETY: `input` is a valid NUL-terminated C string; `open_b`/`close_b` are byte-valued chars.
    let found = unsafe { sys::switch_find_end_paren(base, open_b as c_char, close_b as c_char) };
    if found.is_null() {
        return None;
    }
    // SAFETY: `found` is either null (handled above) or a pointer within `input`'s storage, so the
    // byte difference is well-defined and non-negative.
    let offset = unsafe { found.offset_from(base) };
    if offset < 0 {
        return None;
    }
    Some(offset as usize)
}

#[cfg(all(test, feature = "live_fs"))]
mod tests {
    use super::*;

    #[test]
    fn escape_string_handles_plain() {
        let out = escape_string("hello").unwrap();
        assert!(!out.is_empty());
    }

    #[test]
    fn url_encode_plain_text() {
        let out = url_encode("hello world").unwrap();
        // A space must be percent-encoded.
        assert!(out.contains("%20") || !out.contains(' '), "got: {out}");
    }

    #[test]
    fn url_encode_empty() {
        let out = url_encode("").unwrap();
        assert_eq!(out, "");
    }

    #[test]
    fn format_number_decimal() {
        let out = format_number(1001u64).unwrap();
        assert!(!out.is_empty());
    }

    #[test]
    fn find_end_paren_balanced() {
        // "{a}" -> the closing '}' sits at index 2.
        assert_eq!(find_end_paren("{a}", '{', '}'), Some(2));
    }

    #[test]
    fn find_end_paren_none() {
        assert_eq!(find_end_paren("abc", '{', '}'), None);
        assert_eq!(find_end_paren("{abc", '{', '}'), None);
    }
}

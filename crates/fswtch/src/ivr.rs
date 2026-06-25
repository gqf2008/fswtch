use crate::{Result, Session, cstring, status_to_result, sys};

/// Records the session's media to `path`. `limit` is the maximum recording length in seconds
/// (pass `0` for no limit). A null file handle lets FreeSWITCH open `path` itself.
pub fn record_file(session: Session, path: impl AsRef<str>, limit: u32) -> Result<()> {
    let path = cstring(path)?;
    // SAFETY: `session.as_ptr()` is a live session; `path` is a valid C string; null file handle
    // and input args select the default recording behavior.
    let status = unsafe {
        sys::switch_ivr_record_file(
            session.as_ptr(),
            std::ptr::null_mut(),
            path.as_ptr(),
            std::ptr::null_mut(),
            limit,
        )
    };
    status_to_result(status)
}

/// Parks the session. A `SWITCH_STATUS_FALSE`/break result indicates the channel left the park
/// (typically a hangup); it is surfaced as `Err` like any non-success status.
pub fn park(session: Session) -> Result<()> {
    // SAFETY: `session.as_ptr()` is a live session; a null input args pointer is permitted.
    let status = unsafe { sys::switch_ivr_park(session.as_ptr(), std::ptr::null_mut()) };
    status_to_result(status)
}

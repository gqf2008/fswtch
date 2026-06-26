use std::{error::Error, fmt};

use crate::sys;

pub type Status = sys::switch_status_t;

pub const SUCCESS: Status = sys::switch_status_t::SWITCH_STATUS_SUCCESS;
pub const FALSE: Status = sys::switch_status_t::SWITCH_STATUS_FALSE;
pub const GENERR: Status = sys::switch_status_t::SWITCH_STATUS_GENERR;

/// FreeSWITCH hangup cause (the value returned by originate/bridge and set on channel hangup).
pub type Cause = sys::switch_call_cause_t;

pub const CAUSE_NONE: Cause = sys::switch_call_cause_t_SWITCH_CAUSE_NONE;
pub const CAUSE_NORMAL_CLEARING: Cause = sys::switch_call_cause_t_SWITCH_CAUSE_NORMAL_CLEARING;
pub const CAUSE_USER_BUSY: Cause = sys::switch_call_cause_t_SWITCH_CAUSE_USER_BUSY;
pub const CAUSE_NO_ANSWER: Cause = sys::switch_call_cause_t_SWITCH_CAUSE_NO_ANSWER;
pub const CAUSE_NO_USER_RESPONSE: Cause = sys::switch_call_cause_t_SWITCH_CAUSE_NO_USER_RESPONSE;
pub const CAUSE_ORIGINATOR_CANCEL: Cause = sys::switch_call_cause_t_SWITCH_CAUSE_ORIGINATOR_CANCEL;
pub const CAUSE_RECOVERY_ON_TIMER_EXPIRE: Cause =
    sys::switch_call_cause_t_SWITCH_CAUSE_RECOVERY_ON_TIMER_EXPIRE;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct SwitchError(pub Status);

impl fmt::Display for SwitchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FreeSWITCH returned status {:?}", self.0)
    }
}

impl Error for SwitchError {}

pub type Result<T> = std::result::Result<T, SwitchError>;

pub fn status_to_result(status: Status) -> Result<()> {
    if status == SUCCESS {
        Ok(())
    } else {
        Err(SwitchError(status))
    }
}

pub fn false_on_success(status: Status) -> Status {
    if status == SUCCESS { FALSE } else { status }
}

/// Maps a Rust `bool` to FreeSWITCH's `switch_bool_t` (`SWITCH_TRUE`/`SWITCH_FALSE`).
pub fn switch_bool(value: bool) -> sys::switch_bool_t {
    if value {
        sys::switch_bool_t_SWITCH_TRUE
    } else {
        sys::switch_bool_t_SWITCH_FALSE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_maps_to_ok() {
        assert!(status_to_result(SUCCESS).is_ok());
    }

    #[test]
    fn non_success_maps_to_err() {
        assert!(status_to_result(FALSE).is_err());
        assert!(status_to_result(GENERR).is_err());
    }

    #[test]
    fn false_on_success_inverts_only_success() {
        assert_eq!(false_on_success(SUCCESS), FALSE);
        assert_eq!(false_on_success(FALSE), FALSE);
        assert_eq!(false_on_success(GENERR), GENERR);
    }
}

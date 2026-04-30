use std::{error::Error, fmt};

use crate::sys;

pub type Status = sys::switch_status_t;

pub const SUCCESS: Status = sys::switch_status_t::SWITCH_STATUS_SUCCESS;
pub const FALSE: Status = sys::switch_status_t::SWITCH_STATUS_FALSE;
pub const GENERR: Status = sys::switch_status_t::SWITCH_STATUS_GENERR;

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

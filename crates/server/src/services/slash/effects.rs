//! GM diagnostics for unsupported effects recorded by the effects service.
//! The data is process-local; this command does not modify effect state.

use super::{slash_error, CallError};
use crate::{messages, services::effects::unsupported};

fn valid_args(args: &[&str]) -> bool {
    matches!(args, ["unsupported"])
}

pub(super) fn command(args: &[&str]) -> Result<String, CallError> {
    if !valid_args(args) {
        return Err(slash_error(messages::gm::GM_EFFECTS_USAGE.into()));
    }
    Ok(messages::gm::gm_effects_unsupported(&unsupported::snapshot()))
}

#[cfg(test)]
mod tests;

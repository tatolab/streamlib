// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! How many messages a reader may hold past its input port's mailbox, per
//! inbound link.

use crate::core::error::{Error, Result};

/// The borrow quota an input port takes when it states none.
pub const DEFAULT_INPUT_PORT_BORROW_QUOTA: usize = 2;

/// The largest borrow quota an input port may state. Every channel data
/// service is created to admit it, because iceoryx2 fixes a service's borrow
/// limit when the first opener creates it.
pub const MAX_INPUT_PORT_BORROW_QUOTA: usize = 8;

/// `declared` as an input port's borrow quota, or the default when it states
/// none, refused past [`MAX_INPUT_PORT_BORROW_QUOTA`].
pub fn input_port_borrow_quota(port_name: &str, declared: Option<usize>) -> Result<usize> {
    let quota = declared.unwrap_or(DEFAULT_INPUT_PORT_BORROW_QUOTA);
    if quota > MAX_INPUT_PORT_BORROW_QUOTA {
        return Err(Error::Configuration(format!(
            "input port '{port_name}' declares borrow_quota={quota}, past the most a port may \
             hold, {MAX_INPUT_PORT_BORROW_QUOTA}"
        )));
    }
    Ok(quota)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unstated_quota_is_the_default_and_one_past_the_most_is_refused() {
        assert_eq!(
            input_port_borrow_quota("in", None).unwrap(),
            DEFAULT_INPUT_PORT_BORROW_QUOTA
        );
        assert_eq!(input_port_borrow_quota("in", Some(0)).unwrap(), 0);
        assert_eq!(
            input_port_borrow_quota("in", Some(MAX_INPUT_PORT_BORROW_QUOTA)).unwrap(),
            MAX_INPUT_PORT_BORROW_QUOTA
        );
        let refusal = input_port_borrow_quota("in", Some(MAX_INPUT_PORT_BORROW_QUOTA + 1))
            .unwrap_err()
            .to_string();
        assert!(
            refusal.contains("'in'") && refusal.contains("borrow_quota=9"),
            "{refusal}"
        );
    }
}

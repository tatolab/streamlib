// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The rules every value a caller can state about a runtime obeys.
//!
//! A runtime's name, its mesh's name and its mesh endpoints are all read the
//! same way — the constructor, then an environment variable, then the engine's
//! own default — and all refused the same way. Stated once here so the policy
//! moves as one thing rather than as a copy per value.

use std::ffi::OsString;

use crate::core::error::{Error, Result};

/// What an environment variable says about a configuration value, or nothing.
///
/// An empty value reads as unset, the way an empty `XDG_RUNTIME_DIR` does — the
/// shape a shell leaves behind when a variable is exported and never assigned.
/// A value that is not UTF-8 is refused naming the variable rather than
/// lossily converted, because what a mangled name would then address is
/// nobody's intent.
pub(crate) fn what_an_environment_door_says(
    value: Option<OsString>,
    environment_variable_name: &str,
) -> Result<Option<String>> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    match value.to_str() {
        Some(said) => Ok(Some(said.to_string())),
        None => Err(Error::Configuration(format!(
            "{environment_variable_name} is {:?}, which is not UTF-8",
            value.to_string_lossy()
        ))),
    }
}

/// The refusal a stated configuration value takes: what was said, where it came
/// from, what is wrong with it, and what a legal one looks like.
///
/// Every caller states all four, because a refusal missing any one of them
/// leaves the reader hunting for which door the value came through.
pub(crate) fn refuse_a_stated_configuration_value(
    what_it_was_meant_to_be: &str,
    stated: &str,
    where_it_came_from: &str,
    what_is_wrong: &str,
    what_a_legal_one_may_be: &str,
) -> Error {
    Error::Configuration(format!(
        "{where_it_came_from} is {stated:?}, which is not {what_it_was_meant_to_be}: \
         {what_is_wrong}. {what_a_legal_one_may_be}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;

    /// An exported-but-unassigned variable is no variable.
    #[test]
    fn an_empty_value_reads_as_unset() {
        assert_eq!(
            what_an_environment_door_says(Some(OsString::new()), "STREAMLIB_PROBE")
                .expect("an empty variable is no variable"),
            None
        );
        assert_eq!(
            what_an_environment_door_says(None, "STREAMLIB_PROBE").expect("no variable"),
            None
        );
    }

    /// A value that is there is handed back as it was written.
    #[test]
    fn a_value_reads_back_as_it_was_written() {
        assert_eq!(
            what_an_environment_door_says(Some(OsString::from("desk rig")), "STREAMLIB_PROBE")
                .expect("a legal value"),
            Some("desk rig".to_string())
        );
    }

    /// A value the engine cannot read is refused naming its variable, rather
    /// than lossily converted into something nobody meant.
    #[test]
    fn a_value_that_is_not_utf8_is_refused_naming_its_variable() {
        let refusal = what_an_environment_door_says(
            Some(OsString::from_vec(vec![b'd', 0xff, b'k'])),
            "STREAMLIB_PROBE",
        )
        .expect_err("a value that is not UTF-8 must be refused")
        .to_string();
        assert!(
            refusal.contains("STREAMLIB_PROBE") && refusal.contains("not UTF-8"),
            "{refusal}"
        );
    }

    /// A refusal says all four things, so a reader never has to guess which
    /// door the value came through.
    #[test]
    fn a_refusal_names_the_value_its_door_the_fault_and_the_rule() {
        let refusal = refuse_a_stated_configuration_value(
            "a runtime name",
            "a/b",
            "STREAMLIB_PROBE",
            "it contains '/'",
            "A runtime name is one key chunk",
        )
        .to_string();
        for named in [
            "a runtime name",
            "a/b",
            "STREAMLIB_PROBE",
            "it contains '/'",
            "one key chunk",
        ] {
            assert!(refusal.contains(named), "{refusal} must name {named:?}");
        }
    }
}

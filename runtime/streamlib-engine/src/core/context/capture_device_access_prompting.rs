// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

/// Whether opening a privacy-gated capture device may put the platform's
/// access prompt in front of the user when nobody has answered it yet.
///
/// A prompt answers something the user did: a stream they loaded or started
/// may ask, and a stream the runtime re-loaded by itself at its start may not,
/// so the user is never asked out of nowhere. An access already granted or
/// refused is honoured either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaptureDeviceAccessPrompting {
    /// Someone loaded or started the stream, so the platform may ask them.
    #[default]
    MayAskTheUser,
    /// Nobody acted on the stream, so an access never asked for is refused by
    /// name, naming what to do to be asked, instead of prompting.
    RefuseWhatWasNeverAskedFor,
}

/// The variable a helper process reads its stream's prompting policy from.
pub const CAPTURE_DEVICE_ACCESS_PROMPTING_ENVIRONMENT_VARIABLE: &str =
    "STREAMLIB_CAPTURE_DEVICE_ACCESS_PROMPTING";

impl CaptureDeviceAccessPrompting {
    /// The value [`CAPTURE_DEVICE_ACCESS_PROMPTING_ENVIRONMENT_VARIABLE`]
    /// carries for this policy.
    pub fn as_environment_value(self) -> &'static str {
        match self {
            Self::MayAskTheUser => "may_ask_the_user",
            Self::RefuseWhatWasNeverAskedFor => "refuse_what_was_never_asked_for",
        }
    }

    /// The policy `environment_value` names, if it names one.
    pub fn from_environment_value(environment_value: &str) -> Option<Self> {
        [Self::MayAskTheUser, Self::RefuseWhatWasNeverAskedFor]
            .into_iter()
            .find(|policy| policy.as_environment_value() == environment_value)
    }

    /// The policy a helper process was spawned under. A helper the engine did
    /// not spawn, or one handed a value it does not know, never prompts.
    pub fn of_this_helper_process() -> Self {
        std::env::var(CAPTURE_DEVICE_ACCESS_PROMPTING_ENVIRONMENT_VARIABLE)
            .ok()
            .and_then(|environment_value| Self::from_environment_value(&environment_value))
            .unwrap_or(Self::RefuseWhatWasNeverAskedFor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_policy_reads_back_from_the_value_it_is_spawned_with() {
        for policy in [
            CaptureDeviceAccessPrompting::MayAskTheUser,
            CaptureDeviceAccessPrompting::RefuseWhatWasNeverAskedFor,
        ] {
            assert_eq!(
                CaptureDeviceAccessPrompting::from_environment_value(policy.as_environment_value()),
                Some(policy)
            );
        }
        assert_eq!(
            CaptureDeviceAccessPrompting::from_environment_value("ask"),
            None
        );
    }
}

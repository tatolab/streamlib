// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! How far outside its stream an output port may be read, and the one check
//! every edge a read crosses calls.

use serde::{Deserialize, Serialize};

/// How far outside its stream an output port may be read.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    utoipa::ToSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum OutputPortExposureLevel {
    /// Read by the stream's own nodes alone.
    Internal,
    /// Read by any other stream, and any code, on this machine as well.
    Private,
    /// Read off this machine as well.
    Public,
}

impl std::fmt::Display for OutputPortExposureLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Internal => "internal",
            Self::Private => "private",
            Self::Public => "public",
        })
    }
}

/// Where a reader of an output port reads it from, seen from the port's stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutputPortReaderLocation {
    /// A node of the port's own stream.
    InsideThePortsStream,
    /// Another stream, or other code, on this machine.
    ElsewhereOnThisMachine,
    /// Anything on another machine.
    OnAnotherMachine,
}

impl OutputPortReaderLocation {
    /// The levels a port must be at for a reader here to read it, as a refusal
    /// names them.
    pub fn levels_a_reader_here_may_read(self) -> &'static str {
        match self {
            Self::InsideThePortsStream => "internal, private or public",
            Self::ElsewhereOnThisMachine => "private or public",
            Self::OnAnotherMachine => "public",
        }
    }
}

impl std::fmt::Display for OutputPortReaderLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InsideThePortsStream => "a node of the port's own stream",
            Self::ElsewhereOnThisMachine => "another stream or other code on this machine",
            Self::OnAnotherMachine => "a reader on another machine",
        })
    }
}

/// Whether a port at `level` may be read by a reader at `location`: the one
/// check every edge a read crosses calls.
pub fn output_port_exposure_allows_the_reader(
    level: OutputPortExposureLevel,
    location: OutputPortReaderLocation,
) -> bool {
    use OutputPortExposureLevel::{Internal, Private, Public};
    use OutputPortReaderLocation::{
        ElsewhereOnThisMachine, InsideThePortsStream, OnAnotherMachine,
    };
    match (location, level) {
        (InsideThePortsStream, _) => true,
        (ElsewhereOnThisMachine, Private | Public) => true,
        (ElsewhereOnThisMachine, Internal) => false,
        (OnAnotherMachine, Public) => true,
        (OnAnotherMachine, Internal | Private) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_check_answers_for_every_reader_location_at_every_level() {
        use OutputPortExposureLevel::{Internal, Private, Public};
        use OutputPortReaderLocation::{
            ElsewhereOnThisMachine, InsideThePortsStream, OnAnotherMachine,
        };
        for (location, level, allowed) in [
            (InsideThePortsStream, Internal, true),
            (InsideThePortsStream, Private, true),
            (InsideThePortsStream, Public, true),
            (ElsewhereOnThisMachine, Internal, false),
            (ElsewhereOnThisMachine, Private, true),
            (ElsewhereOnThisMachine, Public, true),
            (OnAnotherMachine, Internal, false),
            (OnAnotherMachine, Private, false),
            (OnAnotherMachine, Public, true),
        ] {
            assert_eq!(
                output_port_exposure_allows_the_reader(level, location),
                allowed,
                "{location} reading a {level} port"
            );
        }
    }

    #[test]
    fn a_level_is_written_in_lowercase_on_the_wire() {
        for (level, written) in [
            (OutputPortExposureLevel::Internal, "\"internal\""),
            (OutputPortExposureLevel::Private, "\"private\""),
            (OutputPortExposureLevel::Public, "\"public\""),
        ] {
            assert_eq!(serde_json::to_string(&level).unwrap(), written);
            assert_eq!(
                serde_json::from_str::<OutputPortExposureLevel>(written).unwrap(),
                level
            );
        }
    }
}

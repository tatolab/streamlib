// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The one root a test build of the runtime and its clients keeps the
//! machine's directories under: the runtime directory, the state directory and,
//! on Linux, the machine runtime lock; and the ports a test build's local relay
//! and loopback HTTP listener bind. On macOS the loopback HTTP listener's bind
//! is the machine runtime lock.
//! Compiled only with the `machine-directories-under-a-test-root` feature,
//! which a release build never enables, so a test build never touches the real
//! machine's.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The environment variable a test build reads its machine root from.
pub const TEST_MACHINE_ROOT_ENVIRONMENT_VARIABLE: &str = "TATOLAB_TEST_MACHINE_ROOT";

/// The environment variable a test build reads its local relay's loopback
/// listener port from; unset, the listener's own default.
pub const TEST_LOCAL_RELAY_LOOPBACK_PORT_ENVIRONMENT_VARIABLE: &str =
    "TATOLAB_TEST_LOCAL_RELAY_LOOPBACK_PORT";

/// The environment variable a test build reads its local relay's
/// every-interface listener port from; unset, the listener's own default.
pub const TEST_LOCAL_RELAY_EVERY_INTERFACE_PORT_ENVIRONMENT_VARIABLE: &str =
    "TATOLAB_TEST_LOCAL_RELAY_EVERY_INTERFACE_PORT";

/// The environment variable a test build reads its loopback HTTP listener's
/// port from; unset, a test build's runtime serves no loopback HTTP on Linux and
/// is refused at the machine runtime lock on macOS.
pub const TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE: &str = "TATOLAB_TEST_LOOPBACK_HTTP_PORT";

/// Why a test build refused its machine root or its loopback HTTP port, naming the variable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TestMachineRootRefusal {
    /// The variable is unset or empty.
    #[error(
        "{TEST_MACHINE_ROOT_ENVIRONMENT_VARIABLE} is not set: this build has the \
         machine-directories-under-a-test-root feature and never touches the real machine's \
         directories; set it to a short absolute path such as /tmp/tl-XXXX"
    )]
    NotSet,
    /// The variable names a relative path.
    #[error("{TEST_MACHINE_ROOT_ENVIRONMENT_VARIABLE} is {value:?}, which is not an absolute path")]
    NotAnAbsolutePath { value: OsString },
    /// The loopback HTTP port's variable is unset or empty.
    #[error(
        "{TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE} is not set: this build has the \
         machine-directories-under-a-test-root feature and never binds the machine's own loopback \
         HTTP port; set it to a free port"
    )]
    LoopbackHttpPortIsNotSet,
    /// The loopback HTTP port's variable names no port from 1 to 65535.
    #[error(
        "{TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE} is {value:?}, which is not a port from 1 \
         to 65535"
    )]
    LoopbackHttpPortIsNotAPort { value: OsString },
}

/// The root every machine directory sits under in this test build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestMachineRoot {
    path: PathBuf,
}

impl TestMachineRoot {
    /// Read the root from [`TEST_MACHINE_ROOT_ENVIRONMENT_VARIABLE`], refusing it unset or relative.
    pub fn from_the_environment() -> Result<Self, TestMachineRootRefusal> {
        test_machine_root_from(std::env::var_os(TEST_MACHINE_ROOT_ENVIRONMENT_VARIABLE))
    }

    /// The root itself.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `<root>/run/`, the runtime directory.
    pub fn runtime_directory(&self) -> PathBuf {
        self.path.join("run")
    }

    /// `<root>/state/`, the state directory.
    pub fn state_directory(&self) -> PathBuf {
        self.path.join("state")
    }

    /// `tatolab-runtime:<root>`, the Linux machine runtime lock's abstract socket name.
    pub fn machine_runtime_lock_abstract_socket_name(&self, production_name: &str) -> String {
        format!("{production_name}:{}", self.path.display())
    }
}

fn test_machine_root_from(
    value: Option<OsString>,
) -> Result<TestMachineRoot, TestMachineRootRefusal> {
    let value = value
        .filter(|value| !value.is_empty())
        .ok_or(TestMachineRootRefusal::NotSet)?;
    let path = PathBuf::from(&value);
    if !path.is_absolute() {
        return Err(TestMachineRootRefusal::NotAnAbsolutePath { value });
    }
    Ok(TestMachineRoot { path })
}

/// The port [`TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE`] names, refusing it unset or not a port.
pub fn test_loopback_http_port_from_the_environment() -> Result<u16, TestMachineRootRefusal> {
    test_loopback_http_port_from(std::env::var_os(
        TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE,
    ))
}

fn test_loopback_http_port_from(value: Option<OsString>) -> Result<u16, TestMachineRootRefusal> {
    let value = value
        .filter(|value| !value.is_empty())
        .ok_or(TestMachineRootRefusal::LoopbackHttpPortIsNotSet)?;
    value
        .to_str()
        .and_then(|port| port.parse::<u16>().ok())
        .filter(|port| *port != 0)
        .ok_or(TestMachineRootRefusal::LoopbackHttpPortIsNotAPort { value })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_or_empty_root_is_refused_naming_the_variable() {
        for value in [None, Some(OsString::new())] {
            let refusal = test_machine_root_from(value).unwrap_err();
            assert_eq!(refusal, TestMachineRootRefusal::NotSet);
            assert!(
                refusal
                    .to_string()
                    .contains(TEST_MACHINE_ROOT_ENVIRONMENT_VARIABLE)
            );
        }
    }

    #[test]
    fn a_relative_root_is_refused_by_name() {
        let refusal = test_machine_root_from(Some(OsString::from("tl-relative"))).unwrap_err();
        assert!(refusal.to_string().contains("\"tl-relative\""), "{refusal}");
        assert!(
            refusal.to_string().contains("not an absolute path"),
            "{refusal}"
        );
    }

    #[test]
    fn every_machine_directory_sits_under_the_root() {
        let root = test_machine_root_from(Some(OsString::from("/tmp/tl-abcd"))).unwrap();
        assert_eq!(root.runtime_directory(), PathBuf::from("/tmp/tl-abcd/run"));
        assert_eq!(root.state_directory(), PathBuf::from("/tmp/tl-abcd/state"));
        assert_eq!(
            root.machine_runtime_lock_abstract_socket_name("tatolab-runtime"),
            "tatolab-runtime:/tmp/tl-abcd"
        );
    }

    #[test]
    fn a_named_port_is_read_and_an_unset_or_unparsable_one_is_refused_naming_the_variable() {
        assert_eq!(
            test_loopback_http_port_from(Some(OsString::from("14446"))),
            Ok(14446)
        );
        for value in [None, Some(OsString::new())] {
            let refusal = test_loopback_http_port_from(value).unwrap_err();
            assert_eq!(refusal, TestMachineRootRefusal::LoopbackHttpPortIsNotSet);
            assert!(
                refusal.to_string().starts_with(&format!(
                    "{TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE} is not set"
                )),
                "{refusal}"
            );
        }
        for value in ["0", "65536", "http", "-1"] {
            let refusal = test_loopback_http_port_from(Some(OsString::from(value))).unwrap_err();
            assert_eq!(
                refusal.to_string(),
                format!(
                    "{TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE} is \"{value}\", which is not a \
                     port from 1 to 65535"
                )
            );
        }
    }
}

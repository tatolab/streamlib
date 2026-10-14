// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The Vulkan device a processor interpreter imports on, named by the runtime
//! at spawn.
//!
//! An OPAQUE_FD import must land on the `deviceUUID` that exported it; the
//! runtime names its own device in the interpreter's environment so the
//! interpreter never resolves one of its own.

use std::ffi::OsString;

use crate::{ConsumerRhiError, Result};

/// The variable the runtime hands a processor interpreter its Vulkan
/// `deviceUUID` in, spelled by [`lowercase_hex_of_vulkan_device_uuid`].
pub const RUNTIME_VULKAN_DEVICE_UUID_ENVIRONMENT_VARIABLE: &str =
    "STREAMLIB_RUNTIME_VULKAN_DEVICE_UUID";

/// The wire spelling of a Vulkan `deviceUUID`: 32 lowercase hex characters.
pub fn lowercase_hex_of_vulkan_device_uuid(vulkan_device_uuid: [u8; 16]) -> String {
    use std::fmt::Write;
    vulkan_device_uuid
        .iter()
        .fold(String::with_capacity(32), |mut lowercase_hex, byte| {
            let _ = write!(lowercase_hex, "{byte:02x}");
            lowercase_hex
        })
}

/// The `deviceUUID` 32 hex characters spell, or `None` for any other text.
pub fn vulkan_device_uuid_from_hex(vulkan_device_uuid_as_hex: &str) -> Option<[u8; 16]> {
    // Digits checked first: `from_str_radix` also takes a leading `+`.
    if vulkan_device_uuid_as_hex.len() != 32
        || !vulkan_device_uuid_as_hex
            .bytes()
            .all(|character| character.is_ascii_hexdigit())
    {
        return None;
    }
    let mut vulkan_device_uuid = [0u8; 16];
    for (byte, hex_pair) in vulkan_device_uuid
        .iter_mut()
        .zip(vulkan_device_uuid_as_hex.as_bytes().chunks_exact(2))
    {
        *byte = u8::from_str_radix(std::str::from_utf8(hex_pair).ok()?, 16).ok()?;
    }
    Some(vulkan_device_uuid)
}

/// Name `vulkan_device_uuid` to the process `command` spawns, as
/// [`runtime_vulkan_device_uuid_named_at_spawn`] reads it there.
pub fn name_the_runtime_vulkan_device_to_a_spawned_process(
    command: &mut std::process::Command,
    vulkan_device_uuid: [u8; 16],
) {
    command.env(
        RUNTIME_VULKAN_DEVICE_UUID_ENVIRONMENT_VARIABLE,
        lowercase_hex_of_vulkan_device_uuid(vulkan_device_uuid),
    );
}

/// The runtime's Vulkan `deviceUUID` from this process's environment, refused
/// by name when the variable is absent or does not spell one.
pub fn runtime_vulkan_device_uuid_named_at_spawn() -> Result<[u8; 16]> {
    runtime_vulkan_device_uuid_from_the_environment_value(std::env::var_os(
        RUNTIME_VULKAN_DEVICE_UUID_ENVIRONMENT_VARIABLE,
    ))
}

fn runtime_vulkan_device_uuid_from_the_environment_value(
    environment_value: Option<OsString>,
) -> Result<[u8; 16]> {
    let Some(environment_value) = environment_value else {
        return Err(ConsumerRhiError::Configuration(format!(
            "{RUNTIME_VULKAN_DEVICE_UUID_ENVIRONMENT_VARIABLE} is not set, so this process does \
             not know which Vulkan device the runtime built on; a processor interpreter is \
             handed it at spawn, and importing on any other device reads the wrong memory"
        )));
    };
    environment_value
        .to_str()
        .and_then(vulkan_device_uuid_from_hex)
        .ok_or_else(|| {
            ConsumerRhiError::Configuration(format!(
                "{RUNTIME_VULKAN_DEVICE_UUID_ENVIRONMENT_VARIABLE} is {environment_value:?}, \
                 which is not a Vulkan device UUID as 32 hex characters"
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const A_DEVICE_UUID: [u8; 16] = [
        0x94, 0x10, 0x5c, 0x9d, 0x1e, 0x3f, 0x69, 0x46, 0xc6, 0x65, 0x7e, 0x58, 0xe8, 0x20, 0x30,
        0x0e,
    ];

    #[test]
    fn a_device_uuid_is_spelled_as_32_lowercase_hex_characters_and_read_back() {
        let spelled = lowercase_hex_of_vulkan_device_uuid(A_DEVICE_UUID);
        assert_eq!(spelled, "94105c9d1e3f6946c6657e58e820300e");
        assert_eq!(vulkan_device_uuid_from_hex(&spelled), Some(A_DEVICE_UUID));
        assert_eq!(
            vulkan_device_uuid_from_hex(&spelled.to_uppercase()),
            Some(A_DEVICE_UUID)
        );
    }

    #[test]
    fn text_that_is_not_32_hex_characters_spells_no_device_uuid() {
        for not_a_device_uuid in [
            "",
            "94105c9d1e3f6946c6657e58e820300",
            "94105c9d1e3f6946c6657e58e820300e0",
            "94105c9d1e3f6946c6657e58e820300g",
            "+4105c9d1e3f6946c6657e58e820300e",
            "94105c9d-1e3f-6946-c665-7e58e820",
            "é4105c9d1e3f6946c6657e58e820300",
        ] {
            assert_eq!(
                vulkan_device_uuid_from_hex(not_a_device_uuid),
                None,
                "{not_a_device_uuid:?}"
            );
        }
    }

    #[test]
    fn the_device_uuid_the_runtime_named_is_read_from_its_variable() {
        assert_eq!(
            runtime_vulkan_device_uuid_from_the_environment_value(Some(
                "94105c9d1e3f6946c6657e58e820300e".into()
            ))
            .expect("a well-formed device UUID"),
            A_DEVICE_UUID
        );
    }

    #[test]
    fn a_spawned_process_is_named_the_device_in_the_spelling_its_reader_takes() {
        let mut command = std::process::Command::new("true");
        name_the_runtime_vulkan_device_to_a_spawned_process(&mut command, A_DEVICE_UUID);
        let named_value = command
            .get_envs()
            .find(|(name, _)| *name == RUNTIME_VULKAN_DEVICE_UUID_ENVIRONMENT_VARIABLE)
            .and_then(|(_, value)| value)
            .map(|value| value.to_owned());
        assert_eq!(
            runtime_vulkan_device_uuid_from_the_environment_value(named_value)
                .expect("the reader takes what the writer named"),
            A_DEVICE_UUID
        );
    }

    #[test]
    fn an_absent_variable_is_refused_naming_it() {
        let refusal = runtime_vulkan_device_uuid_from_the_environment_value(None)
            .expect_err("no variable names no device")
            .to_string();
        assert!(
            refusal.contains(RUNTIME_VULKAN_DEVICE_UUID_ENVIRONMENT_VARIABLE)
                && refusal.contains("not set"),
            "{refusal}"
        );
    }

    #[test]
    fn a_malformed_variable_is_refused_naming_it_and_its_value() {
        let refusal =
            runtime_vulkan_device_uuid_from_the_environment_value(Some("not-a-device-uuid".into()))
                .expect_err("a malformed value names no device")
                .to_string();
        assert!(
            refusal.contains(RUNTIME_VULKAN_DEVICE_UUID_ENVIRONMENT_VARIABLE)
                && refusal.contains("not-a-device-uuid"),
            "{refusal}"
        );
    }
}

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The name a runtime is addressed by on the runtime mesh.
//!
//! It belongs to the runtime rather than to its control plane, is stable across
//! runs of one app, and is one chunk of a port's mesh address
//! `<runtime name>/<display name>/<port>`.

use std::ffi::OsString;
use std::path::Path;

use crate::core::app_directory::resolve_the_app_directory_this_runtime_belongs_to;
use crate::core::error::{Error, Result};
use crate::core::runtime::mesh_address_chunk::{
    CHARACTER_NO_MESH_ADDRESS_CHUNK_MAY_BEGIN_WITH, CHARACTERS_NO_MESH_ADDRESS_CHUNK_MAY_CONTAIN,
    first_reason_this_is_not_one_mesh_address_chunk, what_one_mesh_address_chunk_may_be,
};
use crate::core::stable_short_id::stable_short_id_over;

/// The environment variable that names a runtime when its constructor did not.
pub(crate) const RUNTIME_NAME_ENVIRONMENT_VARIABLE: &str = "STREAMLIB_RUNTIME_NAME";

/// What a forbidden character becomes in a default name.
const REPLACEMENT_FOR_A_CHARACTER_A_DEFAULT_NAME_MAY_NOT_CARRY: char = '-';

/// What stands in for a host whose name this machine would not report.
const HOST_NAME_FOR_A_MACHINE_THAT_REPORTS_NONE: &str = "unknown-host";

/// What stands in for an app directory with no final component (`/`).
const APP_DIRECTORY_NAME_FOR_A_PATH_WITH_NO_FINAL_COMPONENT: &str = "app";

/// The most bytes this reads a host name into. `HOST_NAME_MAX` is 64 on Linux
/// and 255 on Apple; POSIX allows a longer name to be truncated.
const HOST_NAME_BUFFER_BYTES: usize = 256;

/// The name a runtime is addressed by on the mesh.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeName(String);

impl RuntimeName {
    /// The name this runtime takes: the constructor's, else the environment's,
    /// else the default for the app's own directory.
    ///
    /// An empty [`RUNTIME_NAME_ENVIRONMENT_VARIABLE`] reads as unset, the way an
    /// empty `XDG_RUNTIME_DIR` does. An empty name the constructor states is
    /// refused, because stating one is asking for it.
    pub fn from_configuration_environment_or_default(
        configured_runtime_name: Option<String>,
    ) -> Result<Self> {
        resolve_runtime_name(
            configured_runtime_name,
            std::env::var_os(RUNTIME_NAME_ENVIRONMENT_VARIABLE),
            || {
                default_runtime_name_for(
                    &resolve_the_app_directory_this_runtime_belongs_to(),
                    &this_hosts_name(),
                )
            },
        )
    }

    /// The name itself.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RuntimeName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The resolver with every input named, so each arm is testable without
/// reaching into the process's environment.
///
/// The default is a closure rather than a value because building one reads the
/// host name and the working directory, and warns about a host that reports no
/// name — none of which a runtime that was told its name should do.
fn resolve_runtime_name(
    configured_runtime_name: Option<String>,
    runtime_name_from_the_environment: Option<OsString>,
    default_runtime_name: impl FnOnce() -> RuntimeName,
) -> Result<RuntimeName> {
    if let Some(configured) = configured_runtime_name {
        return stated_runtime_name(&configured, "the runtime name it was constructed with");
    }
    if let Some(from_the_environment) =
        runtime_name_from_the_environment.filter(|value| !value.is_empty())
    {
        let Some(from_the_environment) = from_the_environment.to_str() else {
            return Err(refuse_a_stated_runtime_name(
                &from_the_environment.to_string_lossy(),
                RUNTIME_NAME_ENVIRONMENT_VARIABLE,
                "it is not UTF-8",
            ));
        };
        return stated_runtime_name(from_the_environment, RUNTIME_NAME_ENVIRONMENT_VARIABLE);
    }
    Ok(default_runtime_name())
}

/// A name somebody stated, refused unless it is one mesh address chunk.
fn stated_runtime_name(stated: &str, where_it_came_from: &str) -> Result<RuntimeName> {
    match first_reason_this_is_not_one_mesh_address_chunk(stated) {
        None => Ok(RuntimeName(stated.to_string())),
        Some(what_is_wrong) => Err(refuse_a_stated_runtime_name(
            stated,
            where_it_came_from,
            &what_is_wrong,
        )),
    }
}

fn refuse_a_stated_runtime_name(
    stated: &str,
    where_it_came_from: &str,
    what_is_wrong: &str,
) -> Error {
    Error::Configuration(format!(
        "{where_it_came_from} is {stated:?}, which is not a runtime name: {what_is_wrong}. {}",
        what_one_mesh_address_chunk_may_be()
    ))
}

/// `<host name>-<app directory name>-<id>`, every forbidden character replaced.
///
/// The id hashes the directory's **full path**, so two checkouts of one app on
/// one host get different names while every run of one checkout gets the same
/// one.
fn default_runtime_name_for(app_directory: &Path, host_name: &str) -> RuntimeName {
    let app_directory_name = app_directory
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| APP_DIRECTORY_NAME_FOR_A_PATH_WITH_NO_FINAL_COMPONENT.to_string());
    let id = stable_short_id_over(app_directory.as_os_str().as_encoded_bytes());

    RuntimeName(
        replace_every_character_that_would_stop_this_being_one_chunk(&format!(
            "{host_name}-{app_directory_name}-{id}"
        )),
    )
}

/// Make a non-empty `assembled` one legal mesh address chunk by substitution
/// alone — the forbidden characters anywhere, and `@` at the front, where the
/// grammar is the one that forbids it.
fn replace_every_character_that_would_stop_this_being_one_chunk(assembled: &str) -> String {
    assembled
        .char_indices()
        .map(|(index, character)| {
            let forbidden_here = CHARACTERS_NO_MESH_ADDRESS_CHUNK_MAY_CONTAIN.contains(&character)
                || (index == 0 && character == CHARACTER_NO_MESH_ADDRESS_CHUNK_MAY_BEGIN_WITH);
            if forbidden_here {
                REPLACEMENT_FOR_A_CHARACTER_A_DEFAULT_NAME_MAY_NOT_CARRY
            } else {
                character
            }
        })
        .collect()
}

/// This machine's host name, or [`HOST_NAME_FOR_A_MACHINE_THAT_REPORTS_NONE`].
pub(crate) fn this_hosts_name() -> String {
    let mut buffer = vec![0u8; HOST_NAME_BUFFER_BYTES];
    // SAFETY: the buffer is `HOST_NAME_BUFFER_BYTES` long and that is the length
    // passed; `gethostname` writes at most that many bytes.
    let reported = unsafe {
        libc::gethostname(
            buffer.as_mut_ptr() as *mut libc::c_char,
            HOST_NAME_BUFFER_BYTES,
        )
    };
    // POSIX leaves truncation unterminated, so the name ends at the first NUL
    // or at the buffer's end, whichever comes first.
    let host_name = buffer
        .iter()
        .position(|&byte| byte == 0)
        .map(|end| String::from_utf8_lossy(&buffer[..end]).into_owned())
        .unwrap_or_else(|| String::from_utf8_lossy(&buffer).into_owned());

    if reported != 0 || host_name.is_empty() {
        tracing::warn!(
            "this machine reported no host name; unnamed runtimes take \
             '{HOST_NAME_FOR_A_MACHINE_THAT_REPORTS_NONE}' in its place"
        );
        return HOST_NAME_FOR_A_MACHINE_THAT_REPORTS_NONE.to_string();
    }
    host_name
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn resolved(
        configured: Option<&str>,
        from_environment: Option<&str>,
        app_directory: &str,
    ) -> Result<RuntimeName> {
        resolve_runtime_name(
            configured.map(str::to_string),
            from_environment.map(OsString::from),
            || default_runtime_name_for(Path::new(app_directory), "rig"),
        )
    }

    /// The constructor's name wins over the environment's, which wins over the
    /// default.
    #[test]
    fn a_stated_name_beats_the_environment_which_beats_the_default() {
        assert_eq!(
            resolved(
                Some("from-the-constructor"),
                Some("from-the-environment"),
                "/apps/desk"
            )
            .expect("a legal name")
            .as_str(),
            "from-the-constructor"
        );
        assert_eq!(
            resolved(None, Some("from-the-environment"), "/apps/desk")
                .expect("a legal name")
                .as_str(),
            "from-the-environment"
        );
        assert_eq!(
            resolved(None, None, "/apps/desk")
                .expect("a default name")
                .as_str(),
            default_runtime_name_for(Path::new("/apps/desk"), "rig").as_str()
        );
    }

    /// An empty environment value reads as unset; an empty stated one does not.
    #[test]
    fn an_empty_environment_value_reads_as_unset_and_an_empty_stated_one_is_refused() {
        assert_eq!(
            resolved(None, Some(""), "/apps/desk")
                .expect("an empty variable is no variable")
                .as_str(),
            default_runtime_name_for(Path::new("/apps/desk"), "rig").as_str()
        );
        let refusal = resolved(Some(""), None, "/apps/desk")
            .expect_err("a stated empty name must be refused");
        assert!(
            refusal.to_string().contains("it is empty"),
            "the refusal must say what is wrong: {refusal}"
        );
    }

    /// Every forbidden character is refused by name, from either door.
    #[test]
    fn a_stated_name_carrying_a_forbidden_character_is_refused_naming_it() {
        for forbidden in CHARACTERS_NO_MESH_ADDRESS_CHUNK_MAY_CONTAIN {
            let stated = format!("desk{forbidden}rig");
            for (configured, from_environment) in
                [(Some(stated.as_str()), None), (None, Some(stated.as_str()))]
            {
                let refusal = resolved(configured, from_environment, "/apps/desk")
                    .expect_err("a name carrying a forbidden character must be refused");
                assert!(
                    refusal.to_string().contains(&format!("{forbidden:?}")),
                    "the refusal of {stated:?} must name {forbidden:?}: {refusal}"
                );
            }
        }
    }

    /// A stated name beginning with `@` is refused, and the refusal says where
    /// the name came from.
    #[test]
    fn a_stated_name_beginning_with_an_at_sign_is_refused_naming_its_source() {
        let refusal = resolved(None, Some("@runtime"), "/apps/desk")
            .expect_err("a name beginning with '@' must be refused");
        let refusal = refusal.to_string();
        assert!(refusal.contains("begins with '@'"), "{refusal}");
        assert!(
            refusal.contains(RUNTIME_NAME_ENVIRONMENT_VARIABLE),
            "{refusal}"
        );
    }

    /// The default is host, directory name and the path's own id.
    #[test]
    fn the_default_is_the_host_the_directory_name_and_a_hash_of_the_full_path() {
        let name = default_runtime_name_for(Path::new("/home/someone/apps/desk"), "rig");
        let id = stable_short_id_over(b"/home/someone/apps/desk");
        assert_eq!(name.as_str(), format!("rig-desk-{id}"));
    }

    /// Two checkouts sharing a final component get different names; one
    /// directory gets the same name twice.
    #[test]
    fn two_directories_sharing_a_final_component_differ_and_one_directory_repeats() {
        let one = default_runtime_name_for(Path::new("/work/a/desk"), "rig");
        let other = default_runtime_name_for(Path::new("/work/b/desk"), "rig");
        assert_ne!(
            one, other,
            "two checkouts of one app on one host must not collide"
        );
        assert_eq!(
            one,
            default_runtime_name_for(Path::new("/work/a/desk"), "rig"),
            "every run of one checkout takes the same name"
        );
    }

    /// A host name or directory name carrying a forbidden character still
    /// yields a legal default, by substitution.
    #[test]
    fn a_forbidden_character_in_the_host_or_the_directory_is_replaced_rather_than_refused() {
        let name = default_runtime_name_for(Path::new("/apps/desk*lab"), "@rig?one");
        assert_eq!(
            first_reason_this_is_not_one_mesh_address_chunk(name.as_str()),
            None,
            "{name} must be one legal mesh address chunk"
        );
        assert!(name.as_str().starts_with("-rig-one-desk-lab-"), "{name}");
    }

    /// A path with no final component still names something.
    #[test]
    fn a_root_directory_still_yields_a_name() {
        let name = default_runtime_name_for(Path::new("/"), "rig");
        assert_eq!(
            first_reason_this_is_not_one_mesh_address_chunk(name.as_str()),
            None,
            "{name} must be one legal mesh address chunk"
        );
        assert!(
            name.as_str().starts_with(&format!(
                "rig-{APP_DIRECTORY_NAME_FOR_A_PATH_WITH_NO_FINAL_COMPONENT}-"
            )),
            "{name}"
        );
    }

    /// This machine reports a host name that is itself usable in a default.
    #[test]
    fn this_machines_host_name_yields_a_legal_default() {
        let name = default_runtime_name_for(&PathBuf::from("/apps/desk"), &this_hosts_name());
        assert_eq!(
            first_reason_this_is_not_one_mesh_address_chunk(name.as_str()),
            None,
            "{name} must be one legal mesh address chunk"
        );
    }
}

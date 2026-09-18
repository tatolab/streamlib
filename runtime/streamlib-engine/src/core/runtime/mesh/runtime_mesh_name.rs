// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The name of the mesh a runtime joins.
//!
//! Everything a runtime puts on the mesh lives under this name, so two groups
//! sharing one network separate by naming different meshes.

use std::ffi::OsString;

use crate::core::error::{Error, Result};
use crate::iceoryx2::{
    describe_the_one_chunk_grammar, first_reason_this_is_not_one_channel_name_chunk,
};

/// The environment variable that names a runtime's mesh when its constructor
/// did not.
pub(crate) const MESH_NAME_ENVIRONMENT_VARIABLE: &str = "STREAMLIB_MESH_NAME";

/// The mesh a runtime joins when nobody names one, so runtimes find each other
/// out of the box.
const DEFAULT_MESH_NAME: &str = "default";

/// The name of one runtime mesh — one chunk of the channel-name grammar.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeMeshName(String);

impl RuntimeMeshName {
    /// The mesh this runtime joins: the constructor's, else the environment's,
    /// else [`DEFAULT_MESH_NAME`].
    ///
    /// An empty [`MESH_NAME_ENVIRONMENT_VARIABLE`] reads as unset, the way an
    /// empty `XDG_RUNTIME_DIR` does. An empty name the constructor states is
    /// refused, because stating one is asking for it.
    pub fn from_configuration_environment_or_default(
        configured_mesh_name: Option<String>,
    ) -> Result<Self> {
        resolve_mesh_name(
            configured_mesh_name,
            std::env::var_os(MESH_NAME_ENVIRONMENT_VARIABLE),
        )
    }

    /// The name itself.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RuntimeMeshName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The resolver with every input named, so each arm is testable without
/// reaching into the process's environment.
fn resolve_mesh_name(
    configured_mesh_name: Option<String>,
    mesh_name_from_the_environment: Option<OsString>,
) -> Result<RuntimeMeshName> {
    if let Some(configured) = configured_mesh_name {
        return stated_mesh_name(&configured, "the mesh name it was constructed with");
    }
    if let Some(from_the_environment) =
        mesh_name_from_the_environment.filter(|value| !value.is_empty())
    {
        let Some(from_the_environment) = from_the_environment.to_str() else {
            return Err(refuse_a_stated_mesh_name(
                &from_the_environment.to_string_lossy(),
                MESH_NAME_ENVIRONMENT_VARIABLE,
                "it is not UTF-8",
            ));
        };
        return stated_mesh_name(from_the_environment, MESH_NAME_ENVIRONMENT_VARIABLE);
    }
    Ok(RuntimeMeshName(DEFAULT_MESH_NAME.to_string()))
}

/// A name somebody stated, refused unless it is one channel-name chunk.
fn stated_mesh_name(stated: &str, where_it_came_from: &str) -> Result<RuntimeMeshName> {
    match first_reason_this_is_not_one_channel_name_chunk(stated) {
        None => Ok(RuntimeMeshName(stated.to_string())),
        Some(what_is_wrong) => Err(refuse_a_stated_mesh_name(
            stated,
            where_it_came_from,
            &what_is_wrong,
        )),
    }
}

fn refuse_a_stated_mesh_name(stated: &str, where_it_came_from: &str, what_is_wrong: &str) -> Error {
    Error::Configuration(format!(
        "{where_it_came_from} is {stated:?}, which is not a mesh name: {what_is_wrong}. {}",
        describe_the_one_chunk_grammar()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved(
        configured: Option<&str>,
        from_environment: Option<&str>,
    ) -> Result<RuntimeMeshName> {
        resolve_mesh_name(
            configured.map(str::to_string),
            from_environment.map(OsString::from),
        )
    }

    /// The constructor's mesh wins over the environment's, which wins over the
    /// default every runtime joins.
    #[test]
    fn a_stated_mesh_beats_the_environment_which_beats_the_default() {
        assert_eq!(
            resolved(Some("lab"), Some("desk"))
                .expect("a legal name")
                .as_str(),
            "lab"
        );
        assert_eq!(
            resolved(None, Some("desk")).expect("a legal name").as_str(),
            "desk"
        );
        assert_eq!(
            resolved(None, None).expect("the default").as_str(),
            DEFAULT_MESH_NAME
        );
    }

    /// An empty environment value reads as unset; an empty stated one does not.
    #[test]
    fn an_empty_environment_value_reads_as_unset_and_an_empty_stated_one_is_refused() {
        assert_eq!(
            resolved(None, Some(""))
                .expect("an empty variable is no variable")
                .as_str(),
            DEFAULT_MESH_NAME
        );
        let refusal =
            resolved(Some(""), None).expect_err("a stated empty mesh name must be refused");
        assert!(refusal.to_string().contains("empty"), "{refusal}");
    }

    /// The mesh name obeys the channel-name chunk grammar, and a refusal says
    /// both what is wrong and where the name came from.
    #[test]
    fn a_mesh_name_outside_the_chunk_grammar_is_refused_naming_its_source() {
        for stated in [
            "Lab", "9lab", "lab/two", "lab.two", "lab two", "@lab", "lab*",
        ] {
            let refusal = resolved(None, Some(stated))
                .err()
                .unwrap_or_else(|| panic!("{stated:?} must be refused as a mesh name"))
                .to_string();
            assert!(
                refusal.contains(MESH_NAME_ENVIRONMENT_VARIABLE) && refusal.contains(stated),
                "the refusal of {stated:?} must name it and its source: {refusal}"
            );
        }
    }

    /// Lowercase letters, digits, `_` and `-` are the whole legal alphabet.
    #[test]
    fn the_legal_alphabet_is_lowercase_digits_underscore_and_hyphen() {
        for legal in ["default", "lab", "lab-two", "lab_two", "l4b"] {
            assert_eq!(
                resolved(None, Some(legal)).expect("a legal name").as_str(),
                legal
            );
        }
    }
}

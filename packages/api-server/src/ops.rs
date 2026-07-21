// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Runtime-facing control-plane operations shared by the HTTP handlers and the
//! MCP veneer ([`crate::mcp`]).
//!
//! The two front ends speak different wire dialects — HTTP maps an outcome to a
//! status code + JSON body, MCP maps it to a `tools/call` content block — but
//! the runtime work between them (register source → instantiate the first
//! discovered processor → wire the optional connections, with transactional
//! rollback) is identical. That composite lives here exactly once; each front
//! end owns only its own error-to-wire projection.

use std::sync::Arc;

use streamlib::sdk::descriptors::{ModuleIdent, SchemaIdent, SemVer, SemVerRange, TypeName};
use streamlib::sdk::error::Error;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::{
    RegisterProcessorReceipt, ReplaceProcessorFromSource, RuntimeOperations,
    SubmittedProcessorSource,
};

use crate::state::{
    RegisteredPortResponse, RegisteredProcessorPortsResponse, RegistrationOutcome,
    SourceProcessorConnection, SourceProcessorPortRole,
};

/// Composite result of a source submit / replace: the minted registration
/// module, each installed processor's committed port surface, and — for a
/// submit that instantiated the first discovered processor — the instance id
/// and any created connection ids.
pub(crate) struct SubmittedSourceOutcome {
    pub module: String,
    pub processors: Vec<RegisteredProcessorPortsResponse>,
    pub processor_id: Option<String>,
    pub state: RegistrationOutcome,
    pub connections: Vec<String>,
}

/// Staged failure of [`submit_processor_source`]. Each variant carries enough
/// for the front end to render its own wire form: the HTTP handler maps the
/// runtime-`Error` variants through its status mappers and the pre-formatted
/// [`SubmitSourceError::Unprocessable`] message to a 422; the MCP veneer renders
/// any variant as an `isError` text block.
pub(crate) enum SubmitSourceError {
    /// `register_processor_source_async` refused the source (unsupported
    /// language, missing / un-mintable name, build failure).
    Register(Error),
    /// The registration succeeded but instantiation failed for a reason with a
    /// caller-facing explanation already composed (un-mintable instance ident,
    /// or a minted type the runtime did not expose). Always a 422 on HTTP.
    Unprocessable(String),
    /// `add_processor` failed for a generic runtime reason.
    Instantiate(Error),
    /// A `connect` wiring failed; the whole submit was rolled back before this
    /// surfaced, so no orphan node or dangling link remains.
    Connect(Error),
}

/// Staged failure of [`replace_processor_source`].
pub(crate) enum ReplaceSourceError {
    /// `target_session_module` did not parse as an `@org/name@<range>` module
    /// ident. Carries the caller-facing explanation.
    MalformedTargetModule(String),
    /// `replace_processor_async` refused the replacement.
    Replace(Error),
}

/// Register `submitted` as a live `@session/<name>` definition, instantiate the
/// first discovered processor, and apply the optional `connect` wirings. On any
/// wiring failure the whole submit rolls back (disconnect links created earlier
/// in this call, then remove the just-instantiated processor) so the operation
/// is all-or-nothing.
pub(crate) async fn submit_processor_source(
    runtime: &Arc<dyn RuntimeOperations>,
    submitted: SubmittedProcessorSource,
    config: serde_json::Value,
    connect: Vec<SourceProcessorConnection>,
) -> std::result::Result<SubmittedSourceOutcome, SubmitSourceError> {
    let receipt = runtime
        .register_processor_source_async(submitted)
        .await
        .map_err(SubmitSourceError::Register)?;

    let processors = project_receipt_ports(&receipt);
    let module = receipt.module.to_string();

    let Some(first) = receipt.processors.first() else {
        return Ok(SubmittedSourceOutcome {
            module,
            processors,
            processor_id: None,
            state: RegistrationOutcome::Registered,
            connections: Vec::new(),
        });
    };

    let Some(ident) = session_processor_ident(&receipt.module, &first.name) else {
        return Err(SubmitSourceError::Unprocessable(format!(
            "registered module `{module}` yielded an uninstantiable processor identity for type `{}`",
            first.name
        )));
    };

    let processor_id = match runtime
        .add_processor_async(ProcessorSpec::new(ident, config))
        .await
    {
        Ok(id) => id,
        Err(Error::UnknownProcessorType { ident: _ }) => {
            return Err(SubmitSourceError::Unprocessable(format!(
                "registered module `{module}` did not expose processor type `{}` to the runtime",
                first.name
            )));
        }
        Err(error) => return Err(SubmitSourceError::Instantiate(error)),
    };

    let mut created_links = Vec::with_capacity(connect.len());
    for wiring in connect {
        let (from, to) = match wiring.role {
            SourceProcessorPortRole::Output => (
                OutputLinkPortRef::new(processor_id.clone(), wiring.local_port),
                InputLinkPortRef::new(wiring.peer_processor, wiring.peer_port),
            ),
            SourceProcessorPortRole::Input => (
                OutputLinkPortRef::new(wiring.peer_processor, wiring.peer_port),
                InputLinkPortRef::new(processor_id.clone(), wiring.local_port),
            ),
        };
        match runtime.connect_async(from, to).await {
            Ok(link_id) => created_links.push(link_id),
            Err(error) => {
                for created_link_id in &created_links {
                    let _ = runtime.disconnect_async(created_link_id.clone()).await;
                }
                let _ = runtime.remove_processor_async(processor_id.clone()).await;
                return Err(SubmitSourceError::Connect(error));
            }
        }
    }

    let connections = created_links.iter().map(|id| id.to_string()).collect();

    Ok(SubmittedSourceOutcome {
        module,
        processors,
        processor_id: Some(processor_id.to_string()),
        state: RegistrationOutcome::Added,
        connections,
    })
}

/// Swap a live `@session/<name>` source registration for `replacement`,
/// transactionally (a failed replacement restores the prior registration). This
/// is a type-level replacement — running graph instances are not swapped — so
/// the outcome never carries an instance id or connections.
pub(crate) async fn replace_processor_source(
    runtime: &Arc<dyn RuntimeOperations>,
    target_session_module: &str,
    replacement: SubmittedProcessorSource,
) -> std::result::Result<SubmittedSourceOutcome, ReplaceSourceError> {
    use serde::{Deserialize, de::IntoDeserializer};
    let target_session_module: ModuleIdent = match ModuleIdent::deserialize(
        target_session_module.into_deserializer(),
    ) {
        Ok(module) => module,
        Err(error) => {
            let error: serde::de::value::Error = error;
            return Err(ReplaceSourceError::MalformedTargetModule(format!(
                "target_session_module `{target_session_module}` is not a valid `@org/name@<range>` module ident: {error}"
            )));
        }
    };

    let request = ReplaceProcessorFromSource {
        target_session_module,
        replacement,
    };

    let receipt = runtime
        .replace_processor_async(request)
        .await
        .map_err(ReplaceSourceError::Replace)?;

    Ok(SubmittedSourceOutcome {
        module: receipt.module.to_string(),
        processors: project_receipt_ports(&receipt),
        processor_id: None,
        state: RegistrationOutcome::Registered,
        connections: Vec::new(),
    })
}

/// Project a register/replace receipt's committed ports onto the wire response
/// shape (`schema` rendered as `"any"` or `@org/package/Type@version`).
fn project_receipt_ports(
    receipt: &RegisterProcessorReceipt,
) -> Vec<RegisteredProcessorPortsResponse> {
    let project = |ports: &[streamlib::sdk::runtime::RegisteredPortReceipt]| {
        ports
            .iter()
            .map(|port| RegisteredPortResponse {
                name: port.name.clone(),
                schema: port.schema.to_string(),
                delivery_profile: port.delivery_profile.clone(),
            })
            .collect()
    };
    receipt
        .processors
        .iter()
        .map(|processor| RegisteredProcessorPortsResponse {
            name: processor.name.clone(),
            inputs: project(&processor.inputs),
            outputs: project(&processor.outputs),
        })
        .collect()
}

/// The concrete [`SemVer`] a session-module range pins. Session registrations
/// mint an `Exact` range, so the other range shapes fall back to their lower
/// bound and only a wildcard `Any` (never minted for a session) yields `None`.
fn pinned_version(range: &SemVerRange) -> Option<SemVer> {
    match range {
        SemVerRange::Exact(version)
        | SemVerRange::AtLeast(version)
        | SemVerRange::Caret(version)
        | SemVerRange::Tilde(version) => Some(*version),
        SemVerRange::Any => None,
    }
}

/// Build the instantiable [`SchemaIdent`] for a discovered processor `type_name`
/// under the receipt's minted `@org/name@0.0.N` registration module.
fn session_processor_ident(module: &ModuleIdent, type_name: &str) -> Option<SchemaIdent> {
    let r#type = TypeName::new(type_name.to_string()).ok()?;
    let version = pinned_version(&module.version)?;
    Some(SchemaIdent::new(
        module.org.clone(),
        module.name.clone(),
        r#type,
        version,
    ))
}

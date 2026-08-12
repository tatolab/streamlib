// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! [`Runner::add_local`] — register an already-compiled `#[processor]` host
//! type on the one processor registry, live, with no package on disk.
//!
//! The type registers under the class import path its own `#[processor]`
//! descriptor carries. Nothing is minted: identity is derived from the type,
//! never synthesized for the registration, which is the same rule the Python
//! side follows when the wheel registers a class.

use crate::core::descriptors::ProcessorClassImportPath;
use crate::core::error::{Error, Result};
use crate::core::processors::{Config, GeneratedProcessor, PROCESSOR_REGISTRY};

use super::Runner;

impl Runner {
    /// Register host type `P` on the processor registry and return the class
    /// import path [`Runner::add_processor`] names it by.
    ///
    /// `config` is validated against `P::Config` before anything is
    /// registered, so a type whose config does not deserialize is refused
    /// here rather than at instantiation. Registering a type twice is an
    /// error — the registry never overwrites a live registration.
    pub fn add_local<P>(&self, config: serde_json::Value) -> Result<ProcessorClassImportPath>
    where
        P: GeneratedProcessor + 'static,
        P::Config: Config,
    {
        let descriptor = <P as GeneratedProcessor>::descriptor().ok_or_else(|| {
            Error::Configuration(format!(
                "{} exposes no descriptor — it is not a #[processor] type",
                std::any::type_name::<P>()
            ))
        })?;

        serde_json::from_value::<P::Config>(config).map_err(|config_mismatch| {
            Error::Configuration(format!(
                "config does not match {}'s Config type: {config_mismatch}",
                std::any::type_name::<P>()
            ))
        })?;

        let processor_class_import_path = descriptor.processor_class_import_path.clone();
        // Host-compiled Rust types register through the same
        // trait-object path as subprocess hosts.
        let constructor: crate::core::processors::DynamicProcessorConstructorFn = Box::new(
            |node: &crate::core::graph::ProcessorNode| -> Result<
                Box<dyn crate::core::processors::DynGeneratedProcessor + Send>,
            > {
                let config: P::Config = match &node.config {
                    Some(json) => serde_json::from_value(json.clone()).map_err(|e| {
                        Error::Configuration(format!(
                            "config does not match {}'s Config type: {e}",
                            std::any::type_name::<P>()
                        ))
                    })?,
                    None => P::Config::default(),
                };
                Ok(Box::new(P::from_config(config)?))
            },
        );
        PROCESSOR_REGISTRY.register_dynamic(descriptor, constructor)?;

        Ok(processor_class_import_path)
    }
}

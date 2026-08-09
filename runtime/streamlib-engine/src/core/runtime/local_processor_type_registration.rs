// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! [`Runner::add_local`] — register an already-compiled `#[processor]` host
//! type on the one processor registry, live, with no package on disk.
//!
//! The type registers under the identity its own `#[processor]` descriptor
//! carries. Nothing is minted: identity is derived from the type, never
//! synthesized for the registration, which is the same rule the Python side
//! follows when the wheel registers a class by its import path.

use crate::core::error::{Error, Result};
use crate::core::processors::{
    Config, GeneratedProcessor, PROCESSOR_REGISTRY, ProcessorTypeReference,
};

use super::Runner;

impl Runner {
    /// Register host type `P` on the processor registry and return the
    /// reference [`Runner::add_processor`] takes.
    ///
    /// `config` is validated against `P::Config` before anything is
    /// registered, so a type whose config does not deserialize is refused
    /// here rather than at instantiation. Registering a type twice is an
    /// error — the registry never overwrites a live registration.
    pub fn add_local<P>(&self, config: serde_json::Value) -> Result<ProcessorTypeReference>
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

        let identity = descriptor.name.clone();
        let vtable = crate::core::plugin::processor_vtable::vtable_for::<P>();
        PROCESSOR_REGISTRY.register_via_vtable(descriptor, vtable, /* cdylib_resident */ false)?;

        Ok(ProcessorTypeReference::new(
            identity.org,
            identity.package,
            identity.r#type,
        ))
    }
}

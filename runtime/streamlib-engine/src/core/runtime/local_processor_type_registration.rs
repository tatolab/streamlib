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
        serde_json::from_value::<P::Config>(config).map_err(|config_mismatch| {
            Error::Configuration(format!(
                "config does not match {}'s Config type: {config_mismatch}",
                std::any::type_name::<P>()
            ))
        })?;

        PROCESSOR_REGISTRY.register_host_compiled_processor_type::<P>()
    }
}

#[cfg(test)]
mod tests {
    use crate::core::error::Error;
    use crate::core::processors::ProcessorSpec;
    use crate::core::runtime::Runner;

    /// The one setting [`AddLocalSourceTakingOneSetting`] takes.
    #[derive(
        Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
    )]
    #[serde(deny_unknown_fields)]
    pub struct AddLocalSourceTakingOneSettingConfig {
        #[serde(default)]
        pub frame_width: Option<u32>,
    }

    /// A host type registered by [`Runner::add_local`] alone.
    #[crate::processor(
        execution = manual,
        config = crate::core::runtime::local_processor_type_registration::tests::AddLocalSourceTakingOneSettingConfig,
        output("video"),
    )]
    pub struct AddLocalSourceTakingOneSetting;

    impl crate::core::ManualProcessor for AddLocalSourceTakingOneSetting::Processor {
        fn start(
            &mut self,
            _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
        ) -> crate::core::error::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_type_added_locally_refuses_a_setting_its_config_does_not_take_at_add() {
        let runtime = Runner::new().unwrap();
        let source_type = runtime
            .add_local::<AddLocalSourceTakingOneSetting::Processor>(serde_json::json!({}))
            .unwrap();

        let refusal = runtime
            .add_processor(
                ProcessorSpec::new(source_type.clone(), serde_json::json!({"frame_widht": 640}))
                    .with_display_name("front"),
            )
            .unwrap_err();

        match refusal {
            Error::NodeConfigRefused {
                node_name,
                processor_type,
                refusal,
            } => {
                assert_eq!(node_name, "front");
                assert_eq!(processor_type, source_type);
                assert!(refusal.contains("`frame_widht`"), "{refusal}");
            }
            other => panic!("expected NodeConfigRefused, got {other:?}"),
        }
    }
}

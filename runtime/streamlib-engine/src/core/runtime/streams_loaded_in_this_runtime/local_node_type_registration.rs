// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::core::descriptors::NodeClassImportPath;
use crate::core::error::{Error, Result};
use crate::core::nodes::{Config, GeneratedNode, NODE_REGISTRY};

use crate::core::runtime::LoadedStreamInThisRuntime;

impl LoadedStreamInThisRuntime {
    pub fn add_local<P>(&self, config: serde_json::Value) -> Result<NodeClassImportPath>
    where
        P: GeneratedNode + 'static,
        P::Config: Config,
    {
        serde_path_to_error::deserialize::<_, P::Config>(&config).map_err(|config_mismatch| {
            Error::Configuration(format!(
                "config does not match {}'s Config type: {config_mismatch}",
                std::any::type_name::<P>()
            ))
        })?;

        NODE_REGISTRY.register_host_compiled_node_type::<P>()
    }
}

#[cfg(test)]
mod tests {
    use crate::core::error::Error;
    use crate::core::nodes::NodeSpec;
    use crate::core::runtime::Runner;
    use crate::core::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;

    /// The one setting [`AddLocalSourceTakingOneSetting`] takes.
    #[derive(
        Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
    )]
    #[serde(deny_unknown_fields)]
    pub(super) struct AddLocalSourceTakingOneSettingConfig {
        #[serde(default)]
        pub frame_width: Option<u32>,
    }

    /// A host type registered by [`LoadedStreamInThisRuntime::add_local`] alone.
    ///
    /// [`LoadedStreamInThisRuntime::add_local`]: crate::core::runtime::LoadedStreamInThisRuntime::add_local
    #[crate::node(
        execution = manual,
        config = crate::core::runtime::streams_loaded_in_this_runtime::local_node_type_registration::tests::AddLocalSourceTakingOneSettingConfig,
        output("video", pin_type = Texture),
    )]
    pub(super) struct AddLocalSourceTakingOneSetting;

    impl crate::core::ManualNode for AddLocalSourceTakingOneSetting::Node {
        fn start(
            &mut self,
            _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
        ) -> crate::core::error::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_type_added_locally_refuses_a_setting_its_config_does_not_take_at_add() {
        let project_directory = tempfile::tempdir().expect("a project directory");
        let runner = Runner::new().unwrap();
        let stream = runner
            .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
                project_directory.path(),
                "main",
            ))
            .unwrap();
        let source_type = stream
            .add_local::<AddLocalSourceTakingOneSetting::Node>(serde_json::json!({}))
            .unwrap();

        let refusal = stream
            .add_node(
                NodeSpec::new(source_type.clone(), serde_json::json!({"frame_widht": 640}))
                    .with_display_name("front"),
            )
            .unwrap_err();

        match refusal {
            Error::NodeConfigRefused {
                node_name,
                node_type,
                refusal,
            } => {
                assert_eq!(node_name, "front");
                assert_eq!(node_type, source_type);
                assert!(refusal.contains("`frame_widht`"), "{refusal}");
            }
            other => panic!("expected NodeConfigRefused, got {other:?}"),
        }
    }
}

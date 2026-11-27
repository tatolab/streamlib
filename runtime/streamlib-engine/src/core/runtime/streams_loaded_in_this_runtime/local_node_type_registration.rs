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

    /// A host type registered by [`LoadedStreamInThisRuntime::add_local`] alone.
    ///
    /// [`LoadedStreamInThisRuntime::add_local`]: crate::core::runtime::LoadedStreamInThisRuntime::add_local
    #[crate::node(
        execution = manual,
        output("video", pin_type = Texture, formats = ["rgba8_unorm"]),
    )]
    pub(super) fn add_local_source() {}

    /// A Rust node holds no config, so a type added locally refuses every config key at add,
    /// naming it.
    #[test]
    fn a_type_added_locally_refuses_a_config_key_at_add() {
        let project_directory = tempfile::tempdir().expect("a project directory");
        let runner = Runner::new().unwrap();
        let stream = runner
            .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
                project_directory.path(),
                "main",
            ))
            .unwrap();
        let source_type = stream
            .add_local::<AddLocalSource::Node>(serde_json::json!({}))
            .unwrap();

        let refusal = stream
            .add_node(
                NodeSpec::new(source_type.clone(), serde_json::json!({"frame_width": 640}))
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
                assert!(refusal.contains("`frame_width`"), "{refusal}");
            }
            other => panic!("expected NodeConfigRefused, got {other:?}"),
        }
    }
}

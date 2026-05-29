// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Processor and port descriptor types for introspection.

use serde::{Deserialize, Serialize};
pub use streamlib_processor_schema::{
    ModuleIdent, Org, Package, PortSchemaSpec, ProcessorScheduling, SchemaIdent, SemVer,
    SemVerRange, TypeName,
};

/// Lossless wire-format mirror of [`PortSchemaSpec`] used at the cdylib
/// plugin ABI msgpack boundary.
///
/// `PortSchemaSpec`'s default serde impl
/// (`streamlib_processor_schema::PortSchemaSpec`) is intentionally
/// YAML-shaped — `Specific(SchemaIdent)` serializes to just the bare
/// type-name string, and the matching `Deserialize` impl only ever
/// produces `Any | Named`. That contract holds for `streamlib.yaml`
/// authoring, where the bare PascalCase form is canonical and the
/// downstream resolver (`ProjectConfig::resolve_bare_schema_refs` /
/// the `#[streamlib::sdk::processor]` proc-macro) rewrites
/// `Named → Specific` against the enclosing manifest's `schemas:` map.
///
/// It does NOT hold when [`ProcessorDescriptor`] crosses the cdylib
/// dlopen plugin ABI seam via `rmp_serde`. The proc-macro pre-resolves
/// `Named → Specific` at the cdylib's compile time and embeds the
/// fully-qualified [`SchemaIdent`] in the cdylib's `descriptor()`
/// constant. When `host_processor_register` deserializes the msgpack
/// envelope on the host side, the default YAML-shaped impl downcasts
/// the carried `Specific` to `Named(bare type-name)` — the cdylib's
/// resolution work is silently lost, the host stores `Named` in
/// `PROCESSOR_REGISTRY.port_info`, and the WIRE phase panics at
/// `open_iceoryx2_service_op.rs::schema_ident_wire_for_producer`.
///
/// The wire-only mirror below restores the structured-everywhere
/// contract from `docs/architecture/schema-identity-and-packaging.md`
/// Decision 2: every reference to a schema identifier is a structured
/// record on every wire surface. Used via
/// `#[serde(with = "port_schema_spec_wire")]` on
/// [`PortDescriptor::schema`] — every `ProcessorDescriptor` that
/// crosses a serializer boundary now round-trips losslessly.
///
/// YAML authoring (`streamlib.yaml`) continues to use
/// `PortSchemaSpec`'s default impl — `schema: VideoFrame` parses as
/// `Named(VideoFrame)`, then the manifest-side resolver rewrites it
/// to `Specific(SchemaIdent)` before the descriptor is built. The
/// wire mirror only governs the `ProcessorDescriptor` msgpack/JSON
/// hop downstream of that resolution.
pub(crate) mod port_schema_spec_wire {
    use super::{Org, Package, PortSchemaSpec, SchemaIdent, SemVer, TypeName};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    enum Wire {
        Any,
        Named {
            name: String,
        },
        Specific {
            org: String,
            package: String,
            #[serde(rename = "type")]
            type_name: String,
            version_major: u32,
            version_minor: u32,
            version_patch: u32,
        },
    }

    pub fn serialize<S: Serializer>(value: &PortSchemaSpec, s: S) -> Result<S::Ok, S::Error> {
        let wire = match value {
            PortSchemaSpec::Any => Wire::Any,
            PortSchemaSpec::Named(name) => Wire::Named {
                name: name.as_str().to_string(),
            },
            PortSchemaSpec::Specific(ident) => Wire::Specific {
                org: ident.org.as_str().to_string(),
                package: ident.package.as_str().to_string(),
                type_name: ident.r#type.as_str().to_string(),
                version_major: ident.version.major,
                version_minor: ident.version.minor,
                version_patch: ident.version.patch,
            },
        };
        wire.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<PortSchemaSpec, D::Error> {
        use serde::de::Error;
        let wire = Wire::deserialize(d)?;
        Ok(match wire {
            Wire::Any => PortSchemaSpec::Any,
            Wire::Named { name } => {
                let typed = TypeName::new(name).map_err(D::Error::custom)?;
                PortSchemaSpec::Named(typed)
            }
            Wire::Specific {
                org,
                package,
                type_name,
                version_major,
                version_minor,
                version_patch,
            } => {
                let org = Org::new(org).map_err(D::Error::custom)?;
                let package = Package::new(package).map_err(D::Error::custom)?;
                let type_name = TypeName::new(type_name).map_err(D::Error::custom)?;
                let version = SemVer::new(version_major, version_minor, version_patch);
                PortSchemaSpec::Specific(SchemaIdent::new(org, package, type_name, version))
            }
        })
    }
}

/// Runtime environment for a processor.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProcessorRuntime {
    #[default]
    Rust,
    Python,
    #[serde(alias = "deno")]
    TypeScript,
}

/// Describes an input or output port.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortDescriptor {
    pub name: String,
    pub description: String,
    /// Structured schema spec — `Any` (wildcard for MoQ-style ports) or a
    /// fully-qualified [`SchemaIdent`].
    ///
    /// Serialized via [`port_schema_spec_wire`] so the cdylib plugin ABI msgpack
    /// boundary preserves `Specific(SchemaIdent)` round-trip. The default
    /// `PortSchemaSpec` serde impl is YAML-shaped (lossy on `Specific`)
    /// and stays in place for `streamlib.yaml` manifest parsing where
    /// bare-name authoring is canonical.
    #[serde(with = "port_schema_spec_wire")]
    pub schema: PortSchemaSpec,
    pub required: bool,
    /// Whether this port uses iceoryx2 IPC.
    #[serde(default)]
    pub is_iceoryx2: bool,
    /// Producer-side overflow policy declared by an *input* port (the
    /// destination of an iceoryx2 service). `None` defers to the
    /// engine-wide default `drop_oldest` at wire time. Always `None`
    /// on output ports — the producer side reads the destination port's
    /// declaration. See [`crate::iceoryx2::Overflow`].
    #[serde(default)]
    pub overflow: Option<String>,
}

impl PortDescriptor {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        schema: PortSchemaSpec,
        required: bool,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            schema,
            required,
            is_iceoryx2: false,
            overflow: None,
        }
    }

    /// Create a port descriptor for an iceoryx2 port.
    pub fn iceoryx2(
        name: impl Into<String>,
        description: impl Into<String>,
        schema: PortSchemaSpec,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            schema,
            required: true,
            is_iceoryx2: true,
            overflow: None,
        }
    }

    /// Builder-style override for the producer-side overflow policy.
    /// Meaningful only on input ports; engine-side derivation ignores
    /// this on output ports.
    pub fn with_overflow(mut self, overflow: impl Into<String>) -> Self {
        self.overflow = Some(overflow.into());
        self
    }
}

/// Code examples for a processor in different languages.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CodeExamples {
    pub rust: String,
    pub python: String,
    pub typescript: String,
}

/// A configuration field for a processor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigField {
    pub name: String,
    #[serde(rename = "type")]
    pub field_type: String,
    pub required: bool,
    pub description: String,
}

impl ConfigField {
    pub fn new(
        name: impl Into<String>,
        field_type: impl Into<String>,
        required: bool,
        description: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            field_type: field_type.into(),
            required,
            description: description.into(),
        }
    }
}

/// Trait for config structs to provide field metadata for descriptors.
pub trait ConfigDescriptor {
    /// Returns the list of config fields with their types and descriptions.
    fn config_fields() -> Vec<ConfigField>;
}

/// Default implementation for unit type (no config).
impl ConfigDescriptor for () {
    fn config_fields() -> Vec<ConfigField> {
        Vec::new()
    }
}

/// Describes a processor with its ports and configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessorDescriptor {
    /// Structured processor identity — `@org/package/Type@version`.
    pub name: SchemaIdent,
    pub description: String,
    pub version: String,
    pub repository: String,
    /// Runtime environment (Rust, Python, TypeScript).
    #[serde(default)]
    pub runtime: ProcessorRuntime,
    /// Entrypoint for non-Rust runtimes (e.g., "src.blur:BlurProcessor").
    #[serde(default)]
    pub entrypoint: Option<String>,
    /// Reference to config schema (e.g., "com.example.blur.config@1.0.0").
    #[serde(default)]
    pub config_schema: Option<String>,
    /// Declarative scheduling intent sourced from the manifest's
    /// `scheduling:` block. Read by `compiler/scheduling.rs` at thread-spawn
    /// time. Defaults to `Normal` priority + `processor-{id}` thread name.
    #[serde(default)]
    pub scheduling: ProcessorScheduling,
    pub inputs: Vec<PortDescriptor>,
    pub outputs: Vec<PortDescriptor>,
    pub examples: CodeExamples,
}

impl ProcessorDescriptor {
    pub fn new(name: SchemaIdent, description: impl Into<String>) -> Self {
        Self {
            name,
            description: description.into(),
            version: String::new(),
            repository: String::new(),
            runtime: ProcessorRuntime::default(),
            entrypoint: None,
            config_schema: None,
            scheduling: ProcessorScheduling::default(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            examples: CodeExamples::default(),
        }
    }

    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }

    pub fn with_repository(mut self, repository: impl Into<String>) -> Self {
        self.repository = repository.into();
        self
    }

    pub fn with_runtime(mut self, runtime: ProcessorRuntime) -> Self {
        self.runtime = runtime;
        self
    }

    pub fn with_entrypoint(mut self, entrypoint: impl Into<String>) -> Self {
        self.entrypoint = Some(entrypoint.into());
        self
    }

    pub fn with_config_schema(mut self, schema: impl Into<String>) -> Self {
        self.config_schema = Some(schema.into());
        self
    }

    pub fn with_scheduling(mut self, scheduling: ProcessorScheduling) -> Self {
        self.scheduling = scheduling;
        self
    }

    pub fn with_input(mut self, port: PortDescriptor) -> Self {
        self.inputs.push(port);
        self
    }

    pub fn with_output(mut self, port: PortDescriptor) -> Self {
        self.outputs.push(port);
        self
    }

    pub fn with_rust_example(mut self, example: impl Into<String>) -> Self {
        self.examples.rust = example.into();
        self
    }

    pub fn with_python_example(mut self, example: impl Into<String>) -> Self {
        self.examples.python = example.into();
        self
    }

    pub fn with_typescript_example(mut self, example: impl Into<String>) -> Self {
        self.examples.typescript = example.into();
        self
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn to_yaml(&self) -> Result<String, serde_yaml::Error> {
        serde_yaml::to_string(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `PortDescriptor::schema` must round-trip every `PortSchemaSpec`
    /// variant losslessly through `rmp_serde` — that's the cdylib plugin ABI
    /// wire format. A regression here would silently degrade
    /// `Specific(SchemaIdent)` to `Named(bare_type)` at the
    /// plugin ABI; the WIRE phase would then panic at
    /// `open_iceoryx2_service_op::schema_ident_wire_for_producer`.
    /// Mentally revert the `#[serde(with = "port_schema_spec_wire")]`
    /// attribute on `PortDescriptor::schema` and this test fails on
    /// the Specific case.
    #[test]
    fn port_descriptor_schema_msgpack_round_trip_preserves_specific() {
        let ident = SchemaIdent::new(
            Org::new("tatolab").unwrap(),
            Package::new("core").unwrap(),
            TypeName::new("VideoFrame").unwrap(),
            SemVer::new(1, 0, 0),
        );
        let pd = PortDescriptor::new(
            "video",
            "Live video frames",
            PortSchemaSpec::Specific(ident.clone()),
            true,
        );
        let bytes = rmp_serde::to_vec_named(&pd).expect("encode");
        let pd2: PortDescriptor = rmp_serde::from_slice(&bytes).expect("decode");
        match &pd2.schema {
            PortSchemaSpec::Specific(round_tripped) => {
                assert_eq!(
                    round_tripped, &ident,
                    "Specific(SchemaIdent) must round-trip byte-for-byte; got {:?}",
                    round_tripped
                );
            }
            other => panic!(
                "Specific(SchemaIdent) downgraded to {:?} on msgpack round-trip — the \
                 cdylib plugin ABI is silently destroying schema identity",
                other
            ),
        }
    }

    /// `Named` variant must also round-trip — exercised by the YAML
    /// parser path that produces unresolved `Named` for in-process
    /// `ProjectConfig::load` consumers; if the cdylib plugin ABI hop ever
    /// hands a `Named` across, it stays `Named`.
    #[test]
    fn port_descriptor_schema_msgpack_round_trip_preserves_named() {
        let pd = PortDescriptor::new(
            "video",
            "Live video frames",
            PortSchemaSpec::Named(TypeName::new("VideoFrame").unwrap()),
            true,
        );
        let bytes = rmp_serde::to_vec_named(&pd).expect("encode");
        let pd2: PortDescriptor = rmp_serde::from_slice(&bytes).expect("decode");
        match &pd2.schema {
            PortSchemaSpec::Named(name) => {
                assert_eq!(name.as_str(), "VideoFrame");
            }
            other => panic!("Named round-tripped to {:?}", other),
        }
    }

    /// `Any` must round-trip too — it's the wildcard wire identity.
    #[test]
    fn port_descriptor_schema_msgpack_round_trip_preserves_any() {
        let pd = PortDescriptor::new(
            "frames",
            "Wildcard frames",
            PortSchemaSpec::Any,
            false,
        );
        let bytes = rmp_serde::to_vec_named(&pd).expect("encode");
        let pd2: PortDescriptor = rmp_serde::from_slice(&bytes).expect("decode");
        assert!(matches!(pd2.schema, PortSchemaSpec::Any));
    }

    /// End-to-end through `ProcessorDescriptor` — the actual envelope
    /// that crosses the cdylib plugin ABI at
    /// `register_via_callback` / `host_processor_register`. Locks the
    /// invariant at the type the plugin ABI actually serializes, not just
    /// the inner port descriptor.
    #[test]
    fn processor_descriptor_msgpack_round_trip_preserves_port_specific() {
        let ident = SchemaIdent::new(
            Org::new("tatolab").unwrap(),
            Package::new("core").unwrap(),
            TypeName::new("VideoFrame").unwrap(),
            SemVer::new(1, 0, 0),
        );
        let proc_ident = SchemaIdent::new(
            Org::new("tatolab").unwrap(),
            Package::new("camera").unwrap(),
            TypeName::new("Camera").unwrap(),
            SemVer::new(1, 0, 0),
        );
        let mut pd = ProcessorDescriptor::new(proc_ident, "Camera");
        pd.outputs.push(PortDescriptor::new(
            "video",
            "Live video frames",
            PortSchemaSpec::Specific(ident.clone()),
            true,
        ));
        let bytes = rmp_serde::to_vec_named(&pd).expect("encode");
        let pd2: ProcessorDescriptor = rmp_serde::from_slice(&bytes).expect("decode");
        match &pd2.outputs[0].schema {
            PortSchemaSpec::Specific(round_tripped) => {
                assert_eq!(round_tripped, &ident);
            }
            other => panic!(
                "ProcessorDescriptor.outputs[0].schema downgraded to {:?} on plugin ABI \
                 msgpack round-trip — cdylib registration silently destroying identity",
                other
            ),
        }
    }
}

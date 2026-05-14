// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use parking_lot::RwLock;

use crate::core::descriptors::SchemaIdent;
use crate::core::error::{Result, Error};
use crate::core::graph::{PortInfo, ProcessorNode};
use crate::core::processors::{DynGeneratedProcessor, GeneratedProcessor};
use crate::core::pubsub::{topics, Event, RuntimeEvent, PUBSUB};
use crate::core::ProcessorDescriptor;
use streamlib_processor_schema::PortSchemaSpec;

/// A created processor instance for runtime use.
pub type ProcessorInstance = Box<dyn DynGeneratedProcessor + Send>;

/// Types used by macro-generated code. Not for direct use.
pub mod macro_codegen {
    use super::ProcessorInstanceFactory;

    /// Registration entry for auto-registration of processor factories via inventory.
    pub struct FactoryRegistration {
        pub register_fn: fn(&ProcessorInstanceFactory),
    }

    inventory::collect!(FactoryRegistration);
}

/// Factory function signature for creating processor instances.
///
/// Used by `register_dynamic()` for runtime processor registration from plugins.
pub type DynamicProcessorConstructorFn =
    Box<dyn Fn(&ProcessorNode) -> Result<ProcessorInstance> + Send + Sync>;

mod private {
    /// Factory function signature for creating processors (internal alias).
    pub type ConstructorFn = super::DynamicProcessorConstructorFn;
}

/// Result of processor registration.
#[derive(Debug, Clone)]
pub struct RegisterResult {
    /// Number of processors registered.
    pub count: usize,
}

/// Factory for compile-time registered Rust processors.
pub struct ProcessorInstanceFactory {
    constructors: RwLock<HashMap<SchemaIdent, private::ConstructorFn>>,
    port_info: RwLock<HashMap<SchemaIdent, (Vec<PortInfo>, Vec<PortInfo>)>>,
    descriptors: RwLock<HashMap<SchemaIdent, ProcessorDescriptor>>,
    /// Set of port-data-type schema specs ([`PortSchemaSpec`]).
    /// Orthogonal to the processor-identity HashMaps above — tracks the
    /// universe of port schemas any registered processor exposes, for
    /// `known_schemas()` / `is_schema_known()` debugging surface only.
    schemas: RwLock<HashSet<PortSchemaSpec>>,
}

/// Global processor registry for runtime lookups.
/// Auto-registers all processors collected via inventory on first access.
pub static PROCESSOR_REGISTRY: LazyLock<ProcessorInstanceFactory> = LazyLock::new(|| {
    let factory = ProcessorInstanceFactory::new();
    // Auto-register all processors; ignore errors here (Runner::new checks for empty registry)
    for registration in inventory::iter::<macro_codegen::FactoryRegistration> {
        (registration.register_fn)(&factory);
    }
    factory
});

impl Default for ProcessorInstanceFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessorInstanceFactory {
    pub fn new() -> Self {
        Self {
            constructors: RwLock::new(HashMap::new()),
            port_info: RwLock::new(HashMap::new()),
            descriptors: RwLock::new(HashMap::new()),
            schemas: RwLock::new(HashSet::new()),
        }
    }

    /// Register all processors collected via inventory at link time.
    /// Safe to call multiple times - duplicates are skipped. Empty
    /// inventory is a valid state: apps that compose processors via
    /// `load_project()` instead of compile-time inventory have an
    /// empty registry at construction and populate it later.
    pub fn register_all_processors(&self) -> RegisterResult {
        for registration in inventory::iter::<macro_codegen::FactoryRegistration> {
            (registration.register_fn)(self);
        }
        let count = self.constructors.read().len();
        RegisterResult { count }
    }

    /// Register a processor type with its constructor.
    pub fn register<P>(&self)
    where
        P: GeneratedProcessor + 'static,
        P::Config: for<'de> serde::Deserialize<'de> + Default,
    {
        let descriptor = match <P as GeneratedProcessor>::descriptor() {
            Some(d) => d,
            None => {
                tracing::warn!(
                    "Processor {} has no descriptor, skipping registration",
                    std::any::type_name::<P>()
                );
                return;
            }
        };

        let type_name = descriptor.name.clone();

        let inputs: Vec<PortInfo> = descriptor
            .inputs
            .iter()
            .map(|p| PortInfo {
                name: p.name.clone(),
                data_type: p.schema.clone(),
                port_kind: Default::default(),
            })
            .collect();

        let outputs: Vec<PortInfo> = descriptor
            .outputs
            .iter()
            .map(|p| PortInfo {
                name: p.name.clone(),
                data_type: p.schema.clone(),
                port_kind: Default::default(),
            })
            .collect();

        self.port_info
            .write()
            .insert(type_name.clone(), (inputs.clone(), outputs.clone()));

        {
            let mut schemas = self.schemas.write();
            for port in inputs.iter().chain(outputs.iter()) {
                schemas.insert(port.data_type.clone());
            }
        }

        self.descriptors
            .write()
            .insert(type_name.clone(), descriptor);

        let constructor: private::ConstructorFn = Box::new(move |node: &ProcessorNode| {
            let config: P::Config = match &node.config {
                Some(json) => serde_json::from_value(json.clone()).map_err(|e| {
                    Error::Configuration(format!(
                        "Failed to deserialize config for '{}': {}",
                        node.id, e
                    ))
                })?,
                None => P::Config::default(),
            };

            let processor = P::from_config(config)?;
            Ok(Box::new(processor) as ProcessorInstance)
        });

        {
            let mut constructors = self.constructors.write();
            if constructors.contains_key(&type_name) {
                tracing::debug!(
                    "Processor '{}' already registered, skipping duplicate",
                    type_name
                );
                return;
            }
            constructors.insert(type_name.clone(), constructor);
        }

        tracing::info!("[register] new processor type registered '{}'", type_name);

        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeDidRegisterProcessorType {
                processor_type: type_name.clone(),
            }),
        );
    }

    /// Register a processor dynamically at runtime (for plugins).
    ///
    /// Unlike `register<P>()` which requires compile-time knowledge of the processor type,
    /// this method accepts pre-built descriptor and constructor for runtime registration.
    ///
    /// # Arguments
    /// * `descriptor` - Processor metadata including name, ports, and config schema
    /// * `constructor` - Factory function that creates processor instances
    ///
    /// # Returns
    /// Error if a processor with the same name is already registered.
    pub fn register_dynamic(
        &self,
        descriptor: ProcessorDescriptor,
        constructor: private::ConstructorFn,
    ) -> Result<()> {
        let type_name = descriptor.name.clone();

        // Check for duplicate registration
        if self.constructors.read().contains_key(&type_name) {
            return Err(Error::Configuration(format!(
                "Processor '{}' already registered",
                type_name
            )));
        }

        // Build port info from descriptor
        let inputs: Vec<PortInfo> = descriptor
            .inputs
            .iter()
            .map(|p| PortInfo {
                name: p.name.clone(),
                data_type: p.schema.clone(),
                port_kind: Default::default(),
            })
            .collect();

        let outputs: Vec<PortInfo> = descriptor
            .outputs
            .iter()
            .map(|p| PortInfo {
                name: p.name.clone(),
                data_type: p.schema.clone(),
                port_kind: Default::default(),
            })
            .collect();

        self.port_info
            .write()
            .insert(type_name.clone(), (inputs.clone(), outputs.clone()));

        {
            let mut schemas = self.schemas.write();
            for port in inputs.iter().chain(outputs.iter()) {
                schemas.insert(port.data_type.clone());
            }
        }

        self.descriptors
            .write()
            .insert(type_name.clone(), descriptor);

        self.constructors
            .write()
            .insert(type_name.clone(), constructor);

        tracing::info!(
            "[register_dynamic] new processor type registered '{}'",
            type_name
        );

        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeDidRegisterProcessorType {
                processor_type: type_name.clone(),
            }),
        );

        Ok(())
    }

    /// Register a processor descriptor without a constructor.
    ///
    /// Used for subprocess processors (Python, TypeScript) where no Rust-side
    /// `ProcessorInstance` is created. The graph needs the descriptor and port info
    /// for validation and wiring, but `create()` will return an error if called.
    pub fn register_descriptor_only(&self, descriptor: ProcessorDescriptor) -> Result<()> {
        let type_name = descriptor.name.clone();

        if self.descriptors.read().contains_key(&type_name) {
            return Err(Error::Configuration(format!(
                "Processor '{}' already registered",
                type_name
            )));
        }

        let inputs: Vec<PortInfo> = descriptor
            .inputs
            .iter()
            .map(|p| PortInfo {
                name: p.name.clone(),
                data_type: p.schema.clone(),
                port_kind: Default::default(),
            })
            .collect();

        let outputs: Vec<PortInfo> = descriptor
            .outputs
            .iter()
            .map(|p| PortInfo {
                name: p.name.clone(),
                data_type: p.schema.clone(),
                port_kind: Default::default(),
            })
            .collect();

        self.port_info
            .write()
            .insert(type_name.clone(), (inputs.clone(), outputs.clone()));

        {
            let mut schemas = self.schemas.write();
            for port in inputs.iter().chain(outputs.iter()) {
                schemas.insert(port.data_type.clone());
            }
        }

        self.descriptors
            .write()
            .insert(type_name.clone(), descriptor);

        // No constructor registered - create() will fail with ProcessorNotFound,
        // which is correct since subprocess processors are never instantiated in Rust.

        tracing::info!(
            "[register_descriptor_only] subprocess processor type registered '{}'",
            type_name
        );

        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeDidRegisterProcessorType {
                processor_type: type_name.clone(),
            }),
        );

        Ok(())
    }

    pub fn can_create(&self, processor_type: &SchemaIdent) -> bool {
        self.constructors.read().contains_key(processor_type)
    }

    pub fn create(&self, node: &ProcessorNode) -> Result<ProcessorInstance> {
        let constructors = self.constructors.read();
        let constructor = constructors.get(&node.processor_type).ok_or_else(|| {
            Error::ProcessorNotFound(format!(
                "No factory registered for processor type '{}'",
                node.processor_type
            ))
        })?;

        constructor(node)
    }

    pub fn port_info(
        &self,
        processor_type: &SchemaIdent,
    ) -> Option<(Vec<PortInfo>, Vec<PortInfo>)> {
        self.port_info.read().get(processor_type).cloned()
    }

    pub fn is_registered(&self, processor_type: &SchemaIdent) -> bool {
        self.constructors.read().contains_key(processor_type)
    }

    /// Get the descriptor for a processor type, if registered.
    pub fn descriptor(&self, processor_type: &SchemaIdent) -> Option<ProcessorDescriptor> {
        self.descriptors.read().get(processor_type).cloned()
    }

    /// List all registered processor types with their full descriptors.
    pub fn list_registered(&self) -> Vec<ProcessorDescriptor> {
        self.descriptors.read().values().cloned().collect()
    }

    /// Resolve `(org, package, type)` against the registry by picking the
    /// highest-`SemVer` match across all registered idents. Returns
    /// [`Error::UnknownProcessorType`] when nothing matches.
    ///
    /// Iterates over `descriptors` (the truth for registered idents),
    /// not `constructors`, so subprocess-only processors registered via
    /// [`Self::register_descriptor_only`] participate in resolution.
    pub fn resolve_any_version(
        &self,
        org: &crate::core::descriptors::Org,
        package: &crate::core::descriptors::Package,
        type_name: &crate::core::descriptors::TypeName,
    ) -> Result<SchemaIdent> {
        let descriptors = self.descriptors.read();
        let highest = descriptors
            .keys()
            .filter(|id| &id.org == org && &id.package == package && &id.r#type == type_name)
            .max_by_key(|id| id.version.clone())
            .cloned();
        highest.ok_or_else(|| Error::UnknownProcessorType {
            // No version was supplied; we render the search target as
            // `(org, package, type)@0.0.0` so the diagnostic still names
            // the offending tuple. Callers who want the exact "any
            // version" semantics in the message string should match on
            // the variant and re-render.
            ident: SchemaIdent::new(
                org.clone(),
                package.clone(),
                type_name.clone(),
                crate::core::descriptors::SemVer::new(0, 0, 0),
            ),
        })
    }

    /// All known port-schema specs from registered processor ports,
    /// sorted by Display rendering for diff-stable output.
    pub fn known_schemas(&self) -> Vec<PortSchemaSpec> {
        let mut schemas: Vec<PortSchemaSpec> = self.schemas.read().iter().cloned().collect();
        schemas.sort_by(|a, b| a.to_string().cmp(&b.to_string()));
        schemas
    }

    /// Check if a port-schema spec is known from any registered processor port.
    pub fn is_schema_known(&self, schema: &PortSchemaSpec) -> bool {
        self.schemas.read().contains(schema)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::descriptors::{Org, Package, SemVer, TypeName};

    fn ident(org: &str, pkg: &str, ty: &str, v: SemVer) -> SchemaIdent {
        SchemaIdent::new(
            Org::new(org).unwrap(),
            Package::new(pkg).unwrap(),
            TypeName::new(ty).unwrap(),
            v,
        )
    }

    fn unit_descriptor(name: SchemaIdent) -> ProcessorDescriptor {
        ProcessorDescriptor::new(name, "test")
    }

    #[test]
    fn identical_pascal_case_from_different_org_package_pairs_coexist() {
        // Two packages each ship a `Camera` processor — same PascalCase
        // short name, different `(org, package)` pair. Pre-#707 this
        // collided in the `String`-keyed registry; post-#707 the
        // structured key disambiguates them and both registrations
        // succeed cleanly.
        let factory = ProcessorInstanceFactory::new();

        let camera_a = ident("acme", "core", "Camera", SemVer::new(1, 0, 0));
        let camera_b = ident("contoso", "core", "Camera", SemVer::new(1, 0, 0));

        factory
            .register_descriptor_only(unit_descriptor(camera_a.clone()))
            .expect("first Camera must register cleanly");
        factory
            .register_descriptor_only(unit_descriptor(camera_b.clone()))
            .expect(
                "second Camera (different org) must register cleanly — \
                 the structured key disambiguates @acme/core/Camera@1.0.0 \
                 from @contoso/core/Camera@1.0.0",
            );

        assert!(factory.descriptor(&camera_a).is_some());
        assert!(factory.descriptor(&camera_b).is_some());
        assert_eq!(factory.list_registered().len(), 2);
    }

    #[test]
    fn duplicate_full_4_tuple_returns_clear_error() {
        // Two registrations of the SAME structured ident must fail with
        // an actionable error variant — the new typed key doesn't
        // accidentally tolerate exact 4-tuple collisions.
        let factory = ProcessorInstanceFactory::new();
        let id = ident("acme", "core", "Camera", SemVer::new(1, 0, 0));

        factory
            .register_descriptor_only(unit_descriptor(id.clone()))
            .expect("first registration succeeds");

        let err = factory
            .register_descriptor_only(unit_descriptor(id.clone()))
            .expect_err("duplicate 4-tuple must be rejected");

        match err {
            Error::Configuration(msg) => {
                assert!(
                    msg.contains("already registered"),
                    "error must name the collision; got: {msg}"
                );
                // The Display form of the offending ident is in the
                // message — that's what humans need to see.
                assert!(
                    msg.contains("@acme/core/Camera@1.0.0"),
                    "error must render the structured ident; got: {msg}"
                );
            }
            other => panic!("expected Configuration variant; got {other:?}"),
        }
    }

    #[test]
    fn version_difference_disambiguates_otherwise_identical_ident() {
        // Major-version bumps of the same `(org, package, type)` are
        // distinct registrations — locks the package-as-publication-unit
        // invariant from the milestone description.
        let factory = ProcessorInstanceFactory::new();
        let v1 = ident("acme", "core", "Camera", SemVer::new(1, 0, 0));
        let v2 = ident("acme", "core", "Camera", SemVer::new(2, 0, 0));

        factory.register_descriptor_only(unit_descriptor(v1.clone())).unwrap();
        factory.register_descriptor_only(unit_descriptor(v2.clone())).unwrap();

        assert!(factory.descriptor(&v1).is_some());
        assert!(factory.descriptor(&v2).is_some());
    }

    #[test]
    fn resolve_any_version_picks_highest_semver_when_multiple_registered() {
        let factory = ProcessorInstanceFactory::new();
        let org = Org::new("acme").unwrap();
        let pkg = Package::new("core").unwrap();
        let ty = TypeName::new("Camera").unwrap();

        let v1 = SchemaIdent::new(org.clone(), pkg.clone(), ty.clone(), SemVer::new(1, 0, 0));
        let v2 = SchemaIdent::new(org.clone(), pkg.clone(), ty.clone(), SemVer::new(1, 2, 0));
        let v3 = SchemaIdent::new(org.clone(), pkg.clone(), ty.clone(), SemVer::new(2, 0, 0));

        // Insert out of order to prove the resolver picks max, not last-inserted.
        factory.register_descriptor_only(unit_descriptor(v2.clone())).unwrap();
        factory.register_descriptor_only(unit_descriptor(v3.clone())).unwrap();
        factory.register_descriptor_only(unit_descriptor(v1.clone())).unwrap();

        let resolved = factory.resolve_any_version(&org, &pkg, &ty).unwrap();
        assert_eq!(
            resolved, v3,
            "resolve_any_version must return the highest semver"
        );
    }

    #[test]
    fn resolve_any_version_returns_unknown_processor_type_when_nothing_matches() {
        let factory = ProcessorInstanceFactory::new();
        // Register an unrelated ident — must not satisfy the lookup.
        factory
            .register_descriptor_only(unit_descriptor(ident(
                "other",
                "core",
                "Camera",
                SemVer::new(1, 0, 0),
            )))
            .unwrap();

        let org = Org::new("acme").unwrap();
        let pkg = Package::new("core").unwrap();
        let ty = TypeName::new("Camera").unwrap();

        let err = factory.resolve_any_version(&org, &pkg, &ty).unwrap_err();
        match err {
            Error::UnknownProcessorType { ident } => {
                assert_eq!(ident.org, org);
                assert_eq!(ident.package, pkg);
                assert_eq!(ident.r#type, ty);
            }
            other => panic!("expected UnknownProcessorType, got {other:?}"),
        }
    }

    #[test]
    fn resolve_any_version_does_not_cross_org_or_package_or_type_boundaries() {
        let factory = ProcessorInstanceFactory::new();

        // Same type name + version, different (org, package) tuples must
        // not satisfy a lookup against the wrong tuple.
        factory
            .register_descriptor_only(unit_descriptor(ident(
                "acme",
                "core",
                "Camera",
                SemVer::new(1, 0, 0),
            )))
            .unwrap();
        factory
            .register_descriptor_only(unit_descriptor(ident(
                "acme",
                "audio",
                "Camera",
                SemVer::new(9, 9, 9),
            )))
            .unwrap();
        factory
            .register_descriptor_only(unit_descriptor(ident(
                "contoso",
                "core",
                "Camera",
                SemVer::new(9, 9, 9),
            )))
            .unwrap();
        factory
            .register_descriptor_only(unit_descriptor(ident(
                "acme",
                "core",
                "Microphone",
                SemVer::new(9, 9, 9),
            )))
            .unwrap();

        let resolved = factory
            .resolve_any_version(
                &Org::new("acme").unwrap(),
                &Package::new("core").unwrap(),
                &TypeName::new("Camera").unwrap(),
            )
            .unwrap();
        assert_eq!(resolved.version, SemVer::new(1, 0, 0));
    }
}

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

use parking_lot::RwLock;

use crate::core::ProcessorDescriptor;
use crate::core::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use crate::core::descriptors::ProcessorClassImportPath;
use crate::core::error::{Error, Result};
use crate::core::execution::ExecutionConfig;
use crate::core::graph::{PortInfo, ProcessorNode, is_in_exposed_name_cast_form};
use crate::core::processors::{Config, DynGeneratedProcessor, GeneratedProcessor};
use crate::core::pubsub::{Event, PUBSUB, RuntimeEvent, topics};

/// A created processor instance for runtime use.
///
/// Every processor — host-compiled Rust types registered through
/// `register::<P>()` / `add_local::<P>()` and subprocess host wrappers
/// registered through [`ProcessorInstanceFactory::register_dynamic`] —
/// dispatches through a boxed [`DynGeneratedProcessor`] trait object.
///
/// # Iceoryx2 resource ownership (issue #894)
///
/// The host allocates the inner `OutputWriterInner` and
/// `InputMailboxesInner` Arcs at instance-construction time and hands
/// the processor `OutputWriter` / `InputMailboxes` handles over those
/// Arcs via `set_iceoryx2_resources`; connection-wiring code operates
/// on the inner Arc directly.
pub struct ProcessorInstance(Box<dyn DynGeneratedProcessor + Send>);

impl ProcessorInstance {
    /// Wrap a boxed generated processor for runtime dispatch.
    pub(crate) fn new(processor: Box<dyn DynGeneratedProcessor + Send>) -> Self {
        Self(processor)
    }

    /// Run the processor's `setup` lifecycle.
    pub fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.0.__generated_setup(ctx)
    }

    /// Run the processor's `teardown` lifecycle.
    pub fn teardown(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.0.__generated_teardown(ctx)
    }

    /// Run the processor's `on_pause` hook.
    pub fn on_pause(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        self.0.__generated_on_pause(ctx)
    }

    /// Run the processor's `on_resume` hook.
    pub fn on_resume(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        self.0.__generated_on_resume(ctx)
    }

    /// Run one tick of the processor's `process` body.
    pub fn process(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        self.0.process(ctx)
    }

    /// Start a Manual-mode processor. Pure passthrough — `start`/`stop` are
    /// never gate-wrapped (thread_runner calls them directly); a body that
    /// escalates or spawns threads does its own per-call gate management.
    pub fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.0.start(ctx)
    }

    /// Stop a Manual-mode processor. Pure passthrough — see [`Self::start`].
    pub fn stop(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.0.stop(ctx)
    }

    /// Read the processor's execution config.
    pub fn execution_config(&self) -> ExecutionConfig {
        self.0.execution_config()
    }

    pub fn has_iceoryx2_outputs(&self) -> bool {
        self.0.has_iceoryx2_outputs()
    }

    pub fn has_iceoryx2_inputs(&self) -> bool {
        self.0.has_iceoryx2_inputs()
    }

    /// Notice a helper process that died on its own, and take its group with it.
    pub fn detect_and_clean_up_after_an_out_of_process_helper_that_died(&mut self) {
        self.0
            .detect_and_clean_up_after_an_out_of_process_helper_that_died()
    }

    /// Whether this processor has failed unrecoverably.
    pub fn has_failed_unrecoverably(&self) -> bool {
        self.0.has_failed_unrecoverably()
    }

    /// Where this processor's link wiring goes when its iceoryx2 ports live
    /// outside the engine's address space, and `None` when the engine wires it
    /// itself.
    ///
    /// Only a subprocess-host registration can be out of process.
    pub fn out_of_process_link_wiring(&self) -> Option<Arc<super::OutOfProcessLinkWiringEnvelope>> {
        self.0.out_of_process_link_wiring()
    }

    /// Borrow the host-side `OutputWriterInner` Arc this processor
    /// instance is wired to. Returns `None` if the processor has no
    /// output ports.
    ///
    /// Used by the host's connection-wiring path (compiler ops) to
    /// mutate the inner directly via
    /// [`crate::iceoryx2::OutputWriterInner::set_channel_publisher`]
    /// and [`crate::iceoryx2::OutputWriterInner::add_channel_link`].
    pub fn iceoryx2_output_writer_inner(&self) -> Option<Arc<crate::iceoryx2::OutputWriterInner>> {
        self.0.iceoryx2_output_writer_inner()
    }

    /// Borrow the host-side `InputMailboxesInner` Arc this
    /// processor instance is wired to. Returns `None` if the
    /// processor has no input ports.
    ///
    /// Used by the host's wiring + scheduler paths to call
    /// `add_port`, `add_channel_subscriber`, `set_listener`, `listener_fd`,
    /// `drain_listener`, `any_port_has_data`, etc. directly — all
    /// host-side, no plugin ABI hop to the cdylib.
    pub fn iceoryx2_input_mailboxes_inner(
        &self,
    ) -> Option<Arc<crate::iceoryx2::InputMailboxesInner>> {
        self.0.iceoryx2_input_mailboxes_inner()
    }

    /// Install host-allocated iceoryx2 inner Arcs into this
    /// processor instance. Called once by the factory after
    /// `construct` returns; the host owns the Arcs and clones them
    /// into the cdylib via `set_iceoryx2_resources`.
    ///
    /// Returns the resulting error (if any) from the cdylib's
    /// `set_iceoryx2_resources` vtable slot, plus stashes the Arcs
    /// on `self` so subsequent
    /// `iceoryx2_output_writer_inner` / `iceoryx2_input_mailboxes_inner`
    /// calls see them.
    pub fn install_iceoryx2_resources(&mut self) -> Result<()> {
        let needs_outputs = self.has_iceoryx2_outputs();
        let needs_inputs = self.has_iceoryx2_inputs();
        let output_inner =
            needs_outputs.then(|| Arc::new(crate::iceoryx2::OutputWriterInner::new()));
        if let (Some(output_inner), Some(descriptor)) = (&output_inner, self.0.descriptor()) {
            output_inner
                .declare_output_ports(descriptor.outputs.iter().map(|port| port.name.clone()));
        }
        let input_inner =
            needs_inputs.then(|| Arc::new(crate::iceoryx2::InputMailboxesInner::new()));

        {
            let ow = output_inner
                .clone()
                .map(crate::iceoryx2::OutputWriter::from_inner_arc);
            let im = input_inner
                .clone()
                .map(crate::iceoryx2::InputMailboxes::from_inner_arc);
            self.0.set_iceoryx2_resources(ow, im)
        }
    }

    pub fn apply_config_json(&mut self, config_json: &serde_json::Value) -> Result<()> {
        self.0.apply_config_json(config_json)
    }

    pub fn to_runtime_json(&self) -> serde_json::Value {
        self.0.to_runtime_json()
    }

    pub fn config_json(&self) -> serde_json::Value {
        self.0.config_json()
    }

    /// Downcast handle. Used by the host's compiler ops to reach
    /// host-only subprocess host wrappers.
    pub fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self.0.as_any_mut()
    }
}

/// Legacy-path factory function signature used by
/// [`ProcessorInstanceFactory::register_dynamic`] for subprocess
/// host wrappers (Python / Deno) that don't fit the generic vtable
/// monomorphization shape.
pub type DynamicProcessorConstructorFn =
    Box<dyn Fn(&ProcessorNode) -> Result<Box<dyn DynGeneratedProcessor + Send>> + Send + Sync>;

/// Refuse a config the type's own `Config` would not take, naming the node,
/// its type and the setting.
type NodeConfigTakenCheckFn =
    Box<dyn Fn(&str, &ProcessorClassImportPath, &serde_json::Value) -> Result<()> + Send + Sync>;

/// Per-type registration entry the factory stores.
enum RegistrationKind {
    /// `Box<dyn Fn>` closure constructor — the one dispatch shape, used by
    /// host-compiled Rust types and dynamic host wrappers alike. Never
    /// replaced, and never described.
    LegacyDyn {
        constructor: DynamicProcessorConstructorFn,
        /// Present for a typed Rust registration.
        config_taken_check: Option<NodeConfigTakenCheckFn>,
    },
    /// A type a processor interpreter described, whose constructor starts the
    /// processor in one. A later describe of the same type replaces it; its
    /// config is checked by its own config class, where the node runs.
    DescribedInAProcessorInterpreter {
        constructor: DynamicProcessorConstructorFn,
    },
}

impl RegistrationKind {
    fn constructor(&self) -> &DynamicProcessorConstructorFn {
        match self {
            Self::LegacyDyn { constructor, .. }
            | Self::DescribedInAProcessorInterpreter { constructor } => constructor,
        }
    }
}

/// Everything the registry held for one processor type, removed by
/// [`ProcessorInstanceFactory::unregister_processor_types`] and held so a
/// refused `remove_module` can reinstate the registration exactly.
pub(crate) struct UnregisteredProcessorTypeRecord {
    processor_type: ProcessorClassImportPath,
    registration: Option<RegistrationKind>,
    port_info: Option<(Vec<PortInfo>, Vec<PortInfo>)>,
    descriptor: Option<ProcessorDescriptor>,
}

/// Factory for compile-time registered Rust processors.
///
/// Keyed on the import path of the class each processor is — the same string
/// the control plane reports and a helper process imports its class back by.
/// Stored verbatim and never parsed: the key holds Python's `module:Qualname`
/// and Rust's `crate::module::Type` alike, and the engine owns neither grammar.
///
/// `descriptors` is the authority on which paths are claimed, and it is the
/// outer lock: a registration holds it across its duplicate check and its
/// insert, taking `port_info` and `registrations` inside. Anything that needs
/// two of these three must take them in that order.
pub struct ProcessorInstanceFactory {
    registrations: RwLock<HashMap<ProcessorClassImportPath, RegistrationKind>>,
    port_info: RwLock<HashMap<ProcessorClassImportPath, (Vec<PortInfo>, Vec<PortInfo>)>>,
    descriptors: RwLock<HashMap<ProcessorClassImportPath, ProcessorDescriptor>>,
    /// Each built-in compiled out on this floor, with the floors it runs on.
    built_in_node_types_absent_on_this_floor: RwLock<HashMap<ProcessorClassImportPath, String>>,
}

/// Global processor registry for runtime lookups.
///
/// Starts empty. Callers populate it through one of two paths:
///
/// In-process Rust callers invoke
/// [`ProcessorInstanceFactory::register`] (typed) or
/// [`ProcessorInstanceFactory::register_dynamic`] (subprocess host
/// wrappers) directly on the registry.
pub static PROCESSOR_REGISTRY: LazyLock<ProcessorInstanceFactory> =
    LazyLock::new(ProcessorInstanceFactory::new);

impl Default for ProcessorInstanceFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessorInstanceFactory {
    pub fn new() -> Self {
        Self {
            registrations: RwLock::new(HashMap::new()),
            port_info: RwLock::new(HashMap::new()),
            descriptors: RwLock::new(HashMap::new()),
            built_in_node_types_absent_on_this_floor: RwLock::new(HashMap::new()),
        }
    }

    /// Register a node type, storing `P`'s descriptor + port info, and
    /// log rather than return a registration that fails.
    pub fn register<P>(&self)
    where
        P: GeneratedProcessor + 'static,
        P::Config: Config,
    {
        if let Err(e) = self.register_host_compiled_node_type::<P>() {
            tracing::warn!(
                "Node type registration for {} failed: {}",
                std::any::type_name::<P>(),
                e
            );
        }
    }

    /// Register host-compiled Rust type `P` under its descriptor's import
    /// path, returned, with a constructor and a check that refuses a config
    /// `P::Config` would not take before any node of it is added.
    pub(crate) fn register_host_compiled_node_type<P>(&self) -> Result<ProcessorClassImportPath>
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
        let constructor: DynamicProcessorConstructorFn = Box::new(
            |node: &ProcessorNode| -> Result<Box<dyn DynGeneratedProcessor + Send>> {
                let config: P::Config = match &node.config {
                    Some(config) => config_the_node_type_takes(
                        &node.display_name,
                        &node.processor_type,
                        config,
                    )?,
                    None => P::Config::default(),
                };
                Ok(Box::new(P::from_config(config)?))
            },
        );
        let config_taken_check: NodeConfigTakenCheckFn =
            Box::new(|node_name, node_type, config| {
                config_the_node_type_takes::<P::Config>(node_name, node_type, config).map(drop)
            });
        let processor_class_import_path = descriptor.processor_class_import_path.clone();
        self.register_with_constructor(descriptor, constructor, Some(config_taken_check))?;
        Ok(processor_class_import_path)
    }

    /// Register a processor dynamically at runtime with a non-generic
    /// `Box<dyn Fn>` constructor. Used for subprocess host wrappers
    /// (Python / Deno) where the constructor isn't expressible as a
    /// generic `register::<P>()` call.
    ///
    /// # Arguments
    /// * `descriptor` - Processor metadata including name, ports, and config schema
    /// * `constructor` - Factory function that creates processor instances
    ///
    /// # Returns
    /// Error if a processor with the same class import path is already
    /// registered.
    pub fn register_dynamic(
        &self,
        descriptor: ProcessorDescriptor,
        constructor: DynamicProcessorConstructorFn,
    ) -> Result<()> {
        self.register_with_constructor(descriptor, constructor, None)
    }

    fn register_with_constructor(
        &self,
        descriptor: ProcessorDescriptor,
        constructor: DynamicProcessorConstructorFn,
        config_taken_check: Option<NodeConfigTakenCheckFn>,
    ) -> Result<()> {
        refuse_port_names_not_cast_or_declared_twice(&descriptor)?;
        let processor_class_import_path = descriptor.processor_class_import_path.clone();

        let inputs: Vec<PortInfo> = descriptor.inputs.iter().map(PortInfo::from).collect();
        let outputs: Vec<PortInfo> = descriptor.outputs.iter().map(PortInfo::from).collect();

        // The `descriptors` guard spans the check and the claim, so two threads
        // registering one path cannot both pass it. Checked against
        // `descriptors` rather than `registrations` because both registration
        // paths write it, and reading the narrower map would let a
        // constructor-bearing registration displace a descriptor-only one.
        let mut descriptors = self.descriptors.write();
        if descriptors.contains_key(&processor_class_import_path) {
            return Err(duplicate_class_import_path(&processor_class_import_path));
        }

        self.port_info
            .write()
            .insert(processor_class_import_path.clone(), (inputs, outputs));

        self.registrations.write().insert(
            processor_class_import_path.clone(),
            RegistrationKind::LegacyDyn {
                constructor,
                config_taken_check,
            },
        );

        descriptors.insert(processor_class_import_path.clone(), descriptor);
        drop(descriptors);

        // A processor's class is reached by import and nothing else, so "which
        // class is this?" is a question the registration record has to answer;
        // a display name cannot, and by the time a helper fails to import one
        // the app is long past `add`. Named field rather than message text
        // because that is what a log consumer can select on.
        tracing::info!(
            processor_class_import_path = processor_class_import_path.as_str(),
            "[register_dynamic] new processor type registered"
        );

        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeDidRegisterProcessorType {
                processor_type: processor_class_import_path,
            }),
        );

        Ok(())
    }

    /// Register a processor descriptor without a constructor.
    ///
    /// The graph has the descriptor and port info it needs to validate and
    /// wire, and `create()` refuses the type.
    pub fn register_descriptor_only(&self, descriptor: ProcessorDescriptor) -> Result<()> {
        refuse_port_names_not_cast_or_declared_twice(&descriptor)?;
        let processor_class_import_path = descriptor.processor_class_import_path.clone();

        let inputs: Vec<PortInfo> = descriptor.inputs.iter().map(PortInfo::from).collect();
        let outputs: Vec<PortInfo> = descriptor.outputs.iter().map(PortInfo::from).collect();

        // One guard across the check and the claim — see `register_dynamic`.
        let mut descriptors = self.descriptors.write();
        if descriptors.contains_key(&processor_class_import_path) {
            return Err(duplicate_class_import_path(&processor_class_import_path));
        }

        self.port_info
            .write()
            .insert(processor_class_import_path.clone(), (inputs, outputs));

        descriptors.insert(processor_class_import_path.clone(), descriptor);
        drop(descriptors);

        tracing::info!(
            "[register_descriptor_only] processor type registered without a constructor '{}'",
            processor_class_import_path
        );

        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeDidRegisterProcessorType {
                processor_type: processor_class_import_path,
            }),
        );

        Ok(())
    }

    /// Register a type a processor interpreter described, replacing the
    /// registration an earlier describe of it left. A type registered any
    /// other way is never replaced: the registration that arrived first stays.
    pub fn register_a_type_described_in_a_processor_interpreter(
        &self,
        descriptor: ProcessorDescriptor,
        constructor: DynamicProcessorConstructorFn,
    ) -> Result<()> {
        refuse_port_names_not_cast_or_declared_twice(&descriptor)?;
        let processor_class_import_path = descriptor.processor_class_import_path.clone();

        let inputs: Vec<PortInfo> = descriptor.inputs.iter().map(PortInfo::from).collect();
        let outputs: Vec<PortInfo> = descriptor.outputs.iter().map(PortInfo::from).collect();

        // One guard across the check and the claim — see `register_dynamic`.
        let mut descriptors = self.descriptors.write();
        if descriptors.contains_key(&processor_class_import_path)
            && !self.was_described_in_a_processor_interpreter(&processor_class_import_path)
        {
            return Err(duplicate_class_import_path(&processor_class_import_path));
        }

        self.port_info
            .write()
            .insert(processor_class_import_path.clone(), (inputs, outputs));
        self.registrations.write().insert(
            processor_class_import_path.clone(),
            RegistrationKind::DescribedInAProcessorInterpreter { constructor },
        );
        descriptors.insert(processor_class_import_path.clone(), descriptor);
        drop(descriptors);

        tracing::info!(
            processor_class_import_path = processor_class_import_path.as_str(),
            "[register_a_type_described_in_a_processor_interpreter] processor type registered"
        );

        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeDidRegisterProcessorType {
                processor_type: processor_class_import_path,
            }),
        );

        Ok(())
    }

    /// Whether `processor_type`'s registration came from a processor
    /// interpreter's describe.
    pub fn was_described_in_a_processor_interpreter(
        &self,
        processor_type: &ProcessorClassImportPath,
    ) -> bool {
        matches!(
            self.registrations.read().get(processor_type),
            Some(RegistrationKind::DescribedInAProcessorInterpreter { .. })
        )
    }

    /// Whether `processor_type` is registered other than by a describe — a
    /// typed Rust registration, a dynamic one, or a descriptor alone — which no
    /// describe replaces.
    pub fn is_registered_other_than_by_a_describe(
        &self,
        processor_type: &ProcessorClassImportPath,
    ) -> bool {
        self.descriptors.read().contains_key(processor_type)
            && !self.was_described_in_a_processor_interpreter(processor_type)
    }

    /// Remove every registry entry for the given processor types across
    /// all four maps (`registrations`, `port_info`, `descriptors`, plus a
    /// rebuild of the port-schema universe). Returns the removed entries
    /// so a refused `remove_module` can reinstate them exactly via
    /// [`Self::reinstate_unregistered_processor_types`]. Idents with no
    /// entry are skipped.
    pub(crate) fn unregister_processor_types(
        &self,
        processor_class_import_paths: &[ProcessorClassImportPath],
    ) -> Vec<UnregisteredProcessorTypeRecord> {
        let mut removed = Vec::new();
        for import_path in processor_class_import_paths {
            let registration = self.registrations.write().remove(import_path);
            let port_info = self.port_info.write().remove(import_path);
            let descriptor = self.descriptors.write().remove(import_path);
            if registration.is_none() && port_info.is_none() && descriptor.is_none() {
                continue;
            }
            removed.push(UnregisteredProcessorTypeRecord {
                processor_type: import_path.clone(),
                registration,
                port_info,
                descriptor,
            });
        }
        removed
    }

    /// Reinstate entries previously removed by
    /// [`Self::unregister_processor_types`] — the restore half of
    /// `remove_module`'s remove-then-check-then-restore in-use check.
    pub(crate) fn reinstate_unregistered_processor_types(
        &self,
        unregistered: Vec<UnregisteredProcessorTypeRecord>,
    ) {
        if unregistered.is_empty() {
            return;
        }
        for record in unregistered {
            if let Some(registration) = record.registration {
                self.registrations
                    .write()
                    .insert(record.processor_type.clone(), registration);
            }
            if let Some(port_info) = record.port_info {
                self.port_info
                    .write()
                    .insert(record.processor_type.clone(), port_info);
            }
            if let Some(descriptor) = record.descriptor {
                self.descriptors
                    .write()
                    .insert(record.processor_type.clone(), descriptor);
            }
        }
    }

    /// Record a built-in this floor compiles out, so a graph naming it is
    /// refused naming the floors it runs on rather than as a type nobody has.
    pub fn register_built_in_node_type_absent_on_this_floor(
        &self,
        node_type: ProcessorClassImportPath,
        floors_it_runs_on: impl Into<String>,
    ) {
        self.built_in_node_types_absent_on_this_floor
            .write()
            .insert(node_type, floors_it_runs_on.into());
    }

    /// Refuse a built-in this floor compiles out, naming this floor and the
    /// floors it runs on.
    pub fn refuse_a_built_in_node_type_absent_on_this_floor(
        &self,
        node_type: &ProcessorClassImportPath,
    ) -> Result<()> {
        match self
            .built_in_node_types_absent_on_this_floor
            .read()
            .get(node_type)
        {
            Some(floors_it_runs_on) => Err(Error::BuiltInNodeTypeAbsentOnThisFloor {
                node_type: node_type.clone(),
                this_floor: this_floors_name().to_string(),
                floors_it_runs_on: floors_it_runs_on.clone(),
            }),
            None => Ok(()),
        }
    }

    /// Refuse a node this runtime cannot add before anything is added: a
    /// built-in this floor compiles out, or a config its type would not take.
    pub fn refuse_a_node_this_runtime_cannot_add(
        &self,
        node_name: &str,
        node_type: &ProcessorClassImportPath,
        config: &serde_json::Value,
    ) -> Result<()> {
        self.refuse_a_built_in_node_type_absent_on_this_floor(node_type)?;
        self.refuse_a_config_the_node_type_does_not_take(node_name, node_type, config)
    }

    /// Refuse a config the node's type would not take — a setting it does not
    /// know, a value of the wrong kind — naming the node, its type and the
    /// setting. A type registered with no typed config (a Python class) takes
    /// what reaches the interpreter its node runs in, whose config class checks it.
    fn refuse_a_config_the_node_type_does_not_take(
        &self,
        node_name: &str,
        node_type: &ProcessorClassImportPath,
        config: &serde_json::Value,
    ) -> Result<()> {
        match self.registrations.read().get(node_type) {
            Some(RegistrationKind::LegacyDyn {
                config_taken_check: Some(config_taken_check),
                ..
            }) => config_taken_check(node_name, node_type, config),
            _ => Ok(()),
        }
    }

    pub fn can_create(&self, processor_type: &ProcessorClassImportPath) -> bool {
        self.registrations.read().contains_key(processor_type)
    }

    pub fn create(&self, node: &ProcessorNode) -> Result<ProcessorInstance> {
        let registrations = self.registrations.read();
        let registration = registrations.get(&node.processor_type).ok_or_else(|| {
            Error::ProcessorNotFound(format!(
                "No factory registered for processor type '{}'",
                node.processor_type
            ))
        })?;

        let mut instance = ProcessorInstance::new(registration.constructor()(node)?);
        instance.install_iceoryx2_resources()?;
        Ok(instance)
    }

    pub fn port_info(
        &self,
        processor_type: &ProcessorClassImportPath,
    ) -> Option<(Vec<PortInfo>, Vec<PortInfo>)> {
        self.port_info.read().get(processor_type).cloned()
    }

    pub fn is_registered(&self, processor_type: &ProcessorClassImportPath) -> bool {
        self.registrations.read().contains_key(processor_type)
    }

    /// Get the descriptor for a processor type, if registered.
    pub fn descriptor(
        &self,
        processor_type: &ProcessorClassImportPath,
    ) -> Option<ProcessorDescriptor> {
        self.descriptors.read().get(processor_type).cloned()
    }

    /// The class's short name, as the registering surface declared it — what
    /// an instance's display name defaults to.
    ///
    /// Projects the one field out under the read lock rather than going
    /// through [`Self::descriptor`], which clones the whole descriptor: the
    /// snapshot path asks this once per node.
    pub(crate) fn default_display_name(
        &self,
        processor_type: &ProcessorClassImportPath,
    ) -> Option<String> {
        self.descriptors
            .read()
            .get(processor_type)
            .map(|descriptor| descriptor.processor_class_short_name.as_str().to_string())
    }

    /// Every processor class import path the registry holds a descriptor for.
    ///
    /// Projects the keys out under the read lock rather than going through
    /// [`Self::list_registered`], which clones every descriptor whole — port
    /// vectors and config schema included — for callers that want the names.
    pub fn registered_processor_class_import_paths(&self) -> Vec<ProcessorClassImportPath> {
        self.descriptors.read().keys().cloned().collect()
    }

    /// List all registered processor types with their full descriptors.
    pub fn list_registered(&self) -> Vec<ProcessorDescriptor> {
        self.descriptors.read().values().cloned().collect()
    }
}

/// `config` as the `C` a node of `node_type` takes, or the refusal naming
/// the node, its type and the setting serde stopped at.
fn config_the_node_type_takes<C: serde::de::DeserializeOwned>(
    node_name: &str,
    node_type: &ProcessorClassImportPath,
    config: &serde_json::Value,
) -> Result<C> {
    serde_path_to_error::deserialize(config).map_err(|refusal| Error::NodeConfigRefused {
        node_name: node_name.to_string(),
        node_type: node_type.clone(),
        refusal: refusal.to_string(),
    })
}

/// This floor as a person names it.
fn this_floors_name() -> &'static str {
    match std::env::consts::OS {
        "linux" => "Linux",
        "macos" => "macOS",
        "windows" => "Windows",
        other => other,
    }
}

/// The refusal when a second processor claims a class import path the registry
/// already holds.
///
/// One path names one class, so a collision means two declarations that a
/// fresh interpreter or a `use` cannot tell apart — and the remedy differs by
/// language, so the message names both. It never resolves the collision by
/// overwriting: the registration that arrived first stays.
fn duplicate_class_import_path(processor_class_import_path: &ProcessorClassImportPath) -> Error {
    Error::Configuration(format!(
        "two processors both identify as `{processor_class_import_path}`, and one import path \
         names one class. In Python this means the module was loaded twice, so this class object \
         is not the one the registry holds — `importlib.reload` is the usual cause. In Rust it \
         means two `#[processor]` types share a module path, which happens when one is \
         declared inside a function body: a function's name is not part of a module path, so \
         neither type is reachable by `use`. Declare processors at module scope."
    ))
}

/// Refuse a descriptor whose ports are not each in cast form under a name of
/// their own — every lookup casts its argument, so a port registered under an
/// uncast name could never be reached.
fn refuse_port_names_not_cast_or_declared_twice(descriptor: &ProcessorDescriptor) -> Result<()> {
    let mut port_names_seen = HashSet::new();
    for port in descriptor.inputs.iter().chain(&descriptor.outputs) {
        if !is_in_exposed_name_cast_form(&port.name) {
            return Err(Error::DescriptorPortNameNotCast {
                processor_class_import_path: descriptor.processor_class_import_path.clone(),
                port_name: port.name.clone(),
            });
        }
        if !port_names_seen.insert(port.name.as_str()) {
            return Err(Error::DescriptorPortNameDeclaredTwice {
                processor_class_import_path: descriptor.processor_class_import_path.clone(),
                port_name: port.name.clone(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::descriptors::{PortDescriptor, ProcessorClassShortName};

    fn class_import_path(path: &str) -> ProcessorClassImportPath {
        ProcessorClassImportPath::new(path).expect("the fixture path names a class")
    }

    /// The short name is held constant while the import path varies. A test
    /// that varied both together could not tell which one the registry
    /// actually keyed on.
    fn descriptor_for(path: &str) -> ProcessorDescriptor {
        ProcessorDescriptor::new(
            ProcessorClassShortName::new("HeldConstant").unwrap(),
            class_import_path(path),
            "test",
        )
    }

    #[test]
    fn a_processor_is_registered_and_looked_up_by_its_class_import_path() {
        let factory = ProcessorInstanceFactory::new();
        let blur = class_import_path("my_app.filters:BlurProcessor");

        assert!(
            factory.descriptor(&blur).is_none(),
            "nothing is registered until it is"
        );

        factory
            .register_descriptor_only(descriptor_for(blur.as_str()))
            .expect("registering a fresh import path succeeds");

        assert!(factory.descriptor(&blur).is_some());
        assert!(factory.port_info(&blur).is_some());
        assert_eq!(
            factory
                .descriptor(&blur)
                .unwrap()
                .processor_class_import_path,
            blur
        );
    }

    /// The two grammars share one registry and never collide with each other:
    /// the key is the whole string, so nothing canonicalizes a Python path into
    /// a Rust one or back.
    #[test]
    fn distinct_import_paths_do_not_collide() {
        let factory = ProcessorInstanceFactory::new();
        let paths = [
            "my_app.filters:BlurProcessor",
            "my_app::filters::BlurProcessor",
            "other_app.filters:BlurProcessor",
            "my_app.effects:BlurProcessor",
        ];

        for path in paths {
            factory
                .register_descriptor_only(descriptor_for(path))
                .unwrap_or_else(|e| panic!("{path} must register cleanly: {e}"));
        }

        assert_eq!(factory.list_registered().len(), paths.len());
        for path in paths {
            assert!(
                factory.descriptor(&class_import_path(path)).is_some(),
                "{path} must stay addressable by its own path"
            );
        }
    }

    /// The registration that arrived first stays, and the refusal names both
    /// languages' causes — a Rust processor declared in a function body derives
    /// its enclosing module's path, so two of them collide here and nowhere
    /// else.
    #[test]
    fn a_second_class_claiming_a_registered_import_path_is_refused() {
        let factory = ProcessorInstanceFactory::new();
        let path = "my_app.filters:BlurProcessor";

        factory
            .register_descriptor_only(descriptor_for(path))
            .expect("first registration succeeds");

        let refusal = factory
            .register_descriptor_only(descriptor_for(path))
            .expect_err("a duplicate import path must be refused");

        let Error::Configuration(message) = refusal else {
            panic!("expected Configuration; got {refusal:?}");
        };
        assert!(
            message.contains(path),
            "the refusal must name the contested path; got: {message}"
        );
        assert!(
            message.contains("importlib.reload"),
            "the refusal must name the Python cause; got: {message}"
        );
        assert!(
            message.contains("module scope"),
            "the refusal must name the Rust cause and its fix; got: {message}"
        );
        assert_eq!(
            factory.list_registered().len(),
            1,
            "a refused duplicate must not overwrite the live registration"
        );
    }

    /// Two `#[processor]` types declared inside function bodies in one module
    /// derive the same `module_path!()`. Under the structured key their
    /// distinct idents hid the clash and neither was reachable by `use`; the
    /// import-path key surfaces it at registration.
    #[test]
    fn two_rust_processors_sharing_a_module_path_are_caught_at_registration() {
        let factory = ProcessorInstanceFactory::new();
        let shared_module_path = "my_crate::filters";

        factory
            .register_descriptor_only(descriptor_for(shared_module_path))
            .expect("the first of the two registers");

        assert!(
            factory
                .register_descriptor_only(descriptor_for(shared_module_path))
                .is_err(),
            "the second must be refused rather than silently shadowing the first"
        );
    }

    /// `register_dynamic` and `register_descriptor_only` write the same key
    /// into the same maps, so a path claimed through one is claimed against the
    /// other — a Python class cannot quietly displace a native built-in.
    #[test]
    fn the_two_registration_paths_share_one_key_space() {
        let factory = ProcessorInstanceFactory::new();
        let path = "my_app.filters:BlurProcessor";

        factory
            .register_descriptor_only(descriptor_for(path))
            .expect("descriptor-only registration succeeds");

        let constructor: DynamicProcessorConstructorFn =
            Box::new(|_node| Err(Error::Configuration("unreachable".into())));
        assert!(
            factory
                .register_dynamic(descriptor_for(path), constructor)
                .is_err(),
            "a constructor-bearing registration must not overwrite a descriptor-only one"
        );
        assert!(
            !factory.can_create(&class_import_path(path)),
            "the refused registration must not have installed its constructor either"
        );
    }

    /// Two threads racing to claim one path: exactly one wins, and the loser
    /// is refused rather than overwriting. Narrow the `descriptors` guard back
    /// to a `read()` that ends before the inserts and both threads pass the
    /// check, so both insert and the second silently displaces the first.
    #[test]
    fn concurrent_registrations_of_one_path_leave_exactly_one_winner() {
        use std::sync::{Arc, Barrier};

        // Repeated, because losing this race once is a scheduling accident —
        // a single round would pass against the broken code most of the time.
        for _ in 0..200 {
            let factory = Arc::new(ProcessorInstanceFactory::new());
            let both_threads_ready = Arc::new(Barrier::new(2));

            let contenders: Vec<_> = (0..2)
                .map(|_| {
                    let factory = Arc::clone(&factory);
                    let both_threads_ready = Arc::clone(&both_threads_ready);
                    std::thread::spawn(move || {
                        both_threads_ready.wait();
                        factory
                            .register_descriptor_only(descriptor_for(
                                "my_app.filters:RacedProcessor",
                            ))
                            .is_ok()
                    })
                })
                .collect();

            let winners = contenders
                .into_iter()
                .map(|contender| contender.join().expect("no contender may panic"))
                .filter(|registered| *registered)
                .count();

            assert_eq!(winners, 1, "exactly one registration may claim a path");
            assert_eq!(factory.list_registered().len(), 1);
        }
    }

    #[test]
    fn an_unregistered_path_is_absent_from_every_map() {
        let factory = ProcessorInstanceFactory::new();
        factory
            .register_descriptor_only(descriptor_for("my_app.filters:BlurProcessor"))
            .unwrap();

        let never_registered = class_import_path("my_app.filters:SharpenProcessor");
        assert!(factory.descriptor(&never_registered).is_none());
        assert!(factory.port_info(&never_registered).is_none());
        assert!(!factory.is_registered(&never_registered));
        assert!(!factory.can_create(&never_registered));
    }

    fn a_constructor_this_test_never_calls() -> DynamicProcessorConstructorFn {
        Box::new(|_node| {
            Err(Error::NotSupported(
                "this test never constructs the processor".into(),
            ))
        })
    }

    /// A later describe of a type replaces what the earlier one registered —
    /// the class's ports may have changed since — and the registration stays
    /// one a describe made.
    #[test]
    fn a_later_describe_of_a_type_replaces_the_earlier_ones_registration() {
        let factory = ProcessorInstanceFactory::new();
        let path = "my_app.filters:BlurProcessor";

        factory
            .register_a_type_described_in_a_processor_interpreter(
                descriptor_for(path).with_input(port_named("video")),
                a_constructor_this_test_never_calls(),
            )
            .expect("a first describe registers the type");
        factory
            .register_a_type_described_in_a_processor_interpreter(
                descriptor_for(path).with_input(port_named("frames")),
                a_constructor_this_test_never_calls(),
            )
            .expect("a later describe replaces it");

        let (inputs, _) = factory
            .port_info(&class_import_path(path))
            .expect("the type is registered");
        let input_names: Vec<_> = inputs.iter().map(|port| port.name.as_str()).collect();
        assert_eq!(input_names, ["frames"]);
        assert!(factory.was_described_in_a_processor_interpreter(&class_import_path(path)));
        assert!(!factory.is_registered_other_than_by_a_describe(&class_import_path(path)));
    }

    /// A typed Rust registration, a dynamic one and a descriptor alone are
    /// never replaced by a describe claiming their path.
    #[test]
    fn a_describe_never_replaces_a_registration_made_any_other_way() {
        use crate::core::test_support::MockSourceTakingOneSetting;
        let factory = ProcessorInstanceFactory::new();
        factory.register::<MockSourceTakingOneSetting::Processor>();
        let typed = MockSourceTakingOneSetting::processor_class_import_path();
        let dynamic = class_import_path("my_app.filters:RegisteredDynamically");
        factory
            .register_dynamic(
                descriptor_for(dynamic.as_str()),
                a_constructor_this_test_never_calls(),
            )
            .unwrap();
        let descriptor_only = class_import_path("my_app.filters:RegisteredAsADescriptor");
        factory
            .register_descriptor_only(descriptor_for(descriptor_only.as_str()))
            .unwrap();

        for registered_another_way in [&typed, &dynamic, &descriptor_only] {
            assert!(factory.is_registered_other_than_by_a_describe(registered_another_way));
            let refused = factory
                .register_a_type_described_in_a_processor_interpreter(
                    descriptor_for(registered_another_way.as_str()),
                    a_constructor_this_test_never_calls(),
                )
                .expect_err("a describe never replaces another registration");
            assert!(
                refused
                    .to_string()
                    .contains(registered_another_way.as_str()),
                "{refused}"
            );
            assert!(!factory.was_described_in_a_processor_interpreter(registered_another_way));
        }
    }

    fn port_named(name: &str) -> PortDescriptor {
        PortDescriptor {
            name: name.to_string(),
            description: String::new(),
            required: true,
            delivery_profile: None,
            audio_window: None,
        }
    }

    /// Both registration paths refuse one, and nothing of the refused
    /// descriptor stays registered.
    fn assert_refused_by_either_registration(
        descriptor: ProcessorDescriptor,
        is_the_expected_refusal: impl Fn(&Error) -> bool,
    ) {
        let path = descriptor.processor_class_import_path.clone();
        let factory = ProcessorInstanceFactory::new();

        let refusal = factory
            .register_descriptor_only(descriptor.clone())
            .expect_err("register_descriptor_only must refuse it");
        assert!(is_the_expected_refusal(&refusal), "got {refusal:?}");

        let constructor: DynamicProcessorConstructorFn =
            Box::new(|_node| Err(Error::Configuration("unreachable".into())));
        let refusal = factory
            .register_dynamic(descriptor, constructor)
            .expect_err("register_dynamic must refuse it");
        assert!(is_the_expected_refusal(&refusal), "got {refusal:?}");

        assert!(factory.descriptor(&path).is_none());
        assert!(factory.port_info(&path).is_none());
    }

    #[test]
    fn a_descriptor_whose_port_name_is_not_cast_is_refused_by_name() {
        let path = "my_app.filters:BlurProcessor";
        assert_refused_by_either_registration(
            descriptor_for(path).with_output(port_named("videoOut")),
            |refusal| {
                matches!(
                    refusal,
                    Error::DescriptorPortNameNotCast { processor_class_import_path, port_name }
                        if processor_class_import_path.as_str() == path && port_name == "videoOut"
                )
            },
        );
    }

    #[test]
    fn a_descriptor_declaring_one_port_name_twice_is_refused_by_name() {
        let path = "my_app.filters:BlurProcessor";
        assert_refused_by_either_registration(
            descriptor_for(path)
                .with_input(port_named("video"))
                .with_output(port_named("video")),
            |refusal| {
                matches!(
                    refusal,
                    Error::DescriptorPortNameDeclaredTwice { processor_class_import_path, port_name }
                        if processor_class_import_path.as_str() == path && port_name == "video"
                )
            },
        );
    }

    /// The port marker names the port in the author's spelling and resolves to
    /// the cast the descriptor registered.
    #[test]
    fn a_rust_processor_declaring_a_camel_case_port_registers_its_cast() {
        use crate::core::graph::OutputPortMarker;
        use crate::core::test_support::MockProcessorWithACamelCaseOutputPort;

        let factory = ProcessorInstanceFactory::new();
        factory.register::<MockProcessorWithACamelCaseOutputPort::Processor>();

        let descriptor = factory
            .descriptor(&MockProcessorWithACamelCaseOutputPort::processor_class_import_path())
            .expect("the macro-declared processor registers");
        let output_port_names: Vec<&str> = descriptor
            .outputs
            .iter()
            .map(|port| port.name.as_str())
            .collect();
        assert_eq!(output_port_names, ["outone"]);
        assert_eq!(
            <MockProcessorWithACamelCaseOutputPort::OutputLink::outOne as OutputPortMarker>::PORT_NAME,
            "outone"
        );
    }

    #[test]
    fn a_typed_registration_refuses_a_config_naming_the_setting_and_a_python_class_takes_any() {
        use crate::core::test_support::MockSourceTakingOneSetting;
        let factory = ProcessorInstanceFactory::new();
        factory.register::<MockSourceTakingOneSetting::Processor>();
        let python_class = class_import_path("my_app.nodes:Blur");
        factory
            .register_descriptor_only(descriptor_for(python_class.as_str()))
            .unwrap();
        let typed = MockSourceTakingOneSetting::processor_class_import_path();
        let a_misspelled_setting = serde_json::json!({"frame_widht": 640});

        let refusal = factory
            .refuse_a_config_the_node_type_does_not_take("front", &typed, &a_misspelled_setting)
            .unwrap_err()
            .to_string();

        assert!(refusal.contains("node `front`"), "{refusal}");
        assert!(refusal.contains(typed.as_str()), "{refusal}");
        assert!(refusal.contains("`frame_widht`"), "{refusal}");
        let a_setting_of_the_wrong_kind = factory
            .refuse_a_config_the_node_type_does_not_take(
                "front",
                &typed,
                &serde_json::json!({"frame_width": "wide"}),
            )
            .unwrap_err()
            .to_string();
        assert!(
            a_setting_of_the_wrong_kind.contains("frame_width: invalid type"),
            "{a_setting_of_the_wrong_kind}"
        );
        factory
            .refuse_a_config_the_node_type_does_not_take(
                "front",
                &typed,
                &serde_json::json!({"frame_width": 640}),
            )
            .unwrap();
        factory
            .refuse_a_config_the_node_type_does_not_take(
                "blur",
                &python_class,
                &a_misspelled_setting,
            )
            .unwrap();
    }

    #[test]
    fn only_a_type_recorded_absent_on_this_floor_is_refused_as_absent() {
        let factory = ProcessorInstanceFactory::new();
        let compiled_out_here = class_import_path("tatolab.stream:CompiledOutHere");
        factory
            .register_built_in_node_type_absent_on_this_floor(compiled_out_here.clone(), "Plan 9");

        assert!(matches!(
            factory.refuse_a_built_in_node_type_absent_on_this_floor(&compiled_out_here),
            Err(Error::BuiltInNodeTypeAbsentOnThisFloor { .. })
        ));
        factory
            .refuse_a_built_in_node_type_absent_on_this_floor(&class_import_path(
                "tatolab.stream:CameraSource",
            ))
            .unwrap();
    }
}

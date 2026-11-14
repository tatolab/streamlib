// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

use parking_lot::RwLock;

use crate::core::ProcessorDescriptor;
use crate::core::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use crate::core::descriptors::{NodeKind, ProcessorClassImportPath};
use crate::core::error::{Error, Result};
use crate::core::execution::ExecutionConfig;
use crate::core::graph::{PortInfo, ProcessorNode, is_in_exposed_name_cast_form};
use crate::core::processors::{Config, DynGeneratedProcessor, GeneratedProcessor};

/// A created processor instance for runtime use.
///
/// Every processor — host-compiled Rust types registered through
/// `register::<P>()` / `add_local::<P>()` and the processor interpreter hosts
/// registered when a processor interpreter describes their type — dispatches through a boxed [`DynGeneratedProcessor`] trait object.
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

    /// Whether the processor's type declares it paces itself on real time.
    pub fn is_paced_by_real_time(&self) -> bool {
        self.0
            .descriptor()
            .is_some_and(|descriptor| descriptor.paced_by_real_time)
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

    /// Why this processor's node failed itself, once it has.
    pub fn failure_the_node_reported_of_itself(&self) -> Option<String> {
        self.0.failure_the_node_reported_of_itself()
    }

    /// What this processor's `process()` raised as its helper process last reported.
    pub fn why_process_is_raising_as_its_helper_reports(&self) -> Option<String> {
        self.0.why_process_is_raising_as_its_helper_reports()
    }

    /// Why this processor's node is not ready as its helper process last reported.
    pub fn why_the_node_is_not_ready_as_its_helper_reports(&self) -> Option<String> {
        self.0.why_the_node_is_not_ready_as_its_helper_reports()
    }

    /// Why this processor's helper process died on its own, once noticed.
    pub fn why_the_out_of_process_helper_died_on_its_own(&self) -> Option<String> {
        self.0.why_the_out_of_process_helper_died_on_its_own()
    }

    /// Ask to promote the function node whose call lost this processor's
    /// helper out of its islands, before the helper is restarted.
    pub(crate) fn ask_to_promote_the_function_node_that_lost_the_helper_that_died(
        &mut self,
        ctx: &RuntimeContextFullAccess<'_>,
    ) -> Option<super::FunctionNodePromotionAwaitedBeforeTheReaderRestarts> {
        self.0
            .ask_to_promote_the_function_node_that_lost_the_helper_that_died(ctx)
    }

    /// Start a new helper process, with a new processor instance, in place of
    /// one that died on its own.
    pub fn restart_the_out_of_process_helper_that_died(
        &mut self,
        ctx: &RuntimeContextFullAccess<'_>,
    ) -> Result<()> {
        self.0.restart_the_out_of_process_helper_that_died(ctx)
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

    /// Reach `node`'s stream facts from this processor's writer and
    /// mailboxes — the cells `graph` renders and its links read.
    pub(crate) fn reach_the_stream_facts_of(&self, node: &ProcessorNode) {
        if let Some(output_inner) = self.iceoryx2_output_writer_inner() {
            output_inner.reach_stream_facts_of_output_ports(Arc::clone(
                node.stream_facts_of_output_ports(),
            ));
        }
        if let Some(input_inner) = self.iceoryx2_input_mailboxes_inner() {
            input_inner.reach_upstream_stream_facts_by_inbound_link(Arc::clone(
                node.upstream_stream_facts_by_inbound_link(),
            ));
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

/// The constructor a registration builds each processor of its type with.
pub type DynamicProcessorConstructorFn =
    Box<dyn Fn(&ProcessorNode) -> Result<Box<dyn DynGeneratedProcessor + Send>> + Send + Sync>;

/// Refuse a config the type's own `Config` would not take, naming the node,
/// its type and the setting.
type NodeConfigTakenCheckFn =
    Box<dyn Fn(&str, &ProcessorClassImportPath, &serde_json::Value) -> Result<()> + Send + Sync>;

/// What the factory stores for one host-compiled Rust type: its constructor,
/// and the check that refuses a config its `Config` would not take.
struct HostCompiledRustTypeRegistration {
    constructor: DynamicProcessorConstructorFn,
    config_taken_check: NodeConfigTakenCheckFn,
}

/// The natively compiled node types, registered once for the machine.
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
    registrations: RwLock<HashMap<ProcessorClassImportPath, HostCompiledRustTypeRegistration>>,
    port_info: RwLock<HashMap<ProcessorClassImportPath, (Vec<PortInfo>, Vec<PortInfo>)>>,
    descriptors: RwLock<HashMap<ProcessorClassImportPath, ProcessorDescriptor>>,
    /// Each former built-in node type, with the module node that replaced it.
    built_in_node_types_moved_to_modules:
        RwLock<HashMap<ProcessorClassImportPath, ModuleNodeThatReplacedABuiltIn>>,
}

/// The module node type a former built-in became, and the distribution that
/// ships it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ModuleNodeThatReplacedABuiltIn {
    module_node_type: ProcessorClassImportPath,
    module_distribution: String,
}

/// The machine's registry of natively compiled node types.
///
/// Starts empty, and is filled by [`ProcessorInstanceFactory::register`]. A
/// type a processor interpreter describes belongs to the stream it was
/// described into, never here.
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
            built_in_node_types_moved_to_modules: RwLock::new(HashMap::new()),
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
        self.register_with_constructor(
            descriptor,
            HostCompiledRustTypeRegistration {
                constructor,
                config_taken_check,
            },
        )?;
        Ok(processor_class_import_path)
    }

    /// Register `registration` under `descriptor`'s import path, refusing a
    /// path already claimed.
    fn register_with_constructor(
        &self,
        descriptor: ProcessorDescriptor,
        registration: HostCompiledRustTypeRegistration,
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

        self.registrations
            .write()
            .insert(processor_class_import_path.clone(), registration);

        descriptors.insert(processor_class_import_path.clone(), descriptor);
        drop(descriptors);

        // A processor's class is reached by import and nothing else, so "which
        // class is this?" is a question the registration record has to answer;
        // a display name cannot, and by the time a helper fails to import one
        // the app is long past `add`. Named field rather than message text
        // because that is what a log consumer can select on.
        tracing::info!(
            processor_class_import_path = processor_class_import_path.as_str(),
            "new processor type registered"
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

        // One guard across the check and the claim — see
        // `register_with_constructor`.
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

        Ok(())
    }

    /// Record a built-in that left the runtime for a module, so a graph naming
    /// it is refused naming the module's node type and the distribution to
    /// install.
    pub fn register_built_in_node_type_moved_to_a_module(
        &self,
        node_type: ProcessorClassImportPath,
        module_node_type: ProcessorClassImportPath,
        module_distribution: impl Into<String>,
    ) {
        self.built_in_node_types_moved_to_modules.write().insert(
            node_type,
            ModuleNodeThatReplacedABuiltIn {
                module_node_type,
                module_distribution: module_distribution.into(),
            },
        );
    }

    /// Refuse a built-in that moved to a module, naming the module node type
    /// and the distribution to install.
    pub fn refuse_a_built_in_node_type_this_runtime_lacks(
        &self,
        node_type: &ProcessorClassImportPath,
    ) -> Result<()> {
        match self
            .built_in_node_types_moved_to_modules
            .read()
            .get(node_type)
        {
            Some(ModuleNodeThatReplacedABuiltIn {
                module_node_type,
                module_distribution,
            }) => Err(Error::BuiltInNodeTypeMovedToAModule {
                node_type: node_type.clone(),
                module_node_type: module_node_type.clone(),
                module_distribution: module_distribution.clone(),
            }),
            None => Ok(()),
        }
    }

    /// Refuse a node this runtime cannot add before anything is added: a
    /// built-in this runtime lacks, or a config its type would not take.
    pub fn refuse_a_node_this_runtime_cannot_add(
        &self,
        node_name: &str,
        node_type: &ProcessorClassImportPath,
        config: &serde_json::Value,
    ) -> Result<()> {
        self.refuse_a_built_in_node_type_this_runtime_lacks(node_type)?;
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
            Some(registration) => (registration.config_taken_check)(node_name, node_type, config),
            None => Ok(()),
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

        let mut instance = ProcessorInstance::new((registration.constructor)(node)?);
        instance.install_iceoryx2_resources()?;
        instance.reach_the_stream_facts_of(node);
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

    /// How `processor_type` runs, projected out under the read lock.
    pub(crate) fn node_kind(&self, processor_type: &ProcessorClassImportPath) -> Option<NodeKind> {
        self.descriptors
            .read()
            .get(processor_type)
            .map(|descriptor| descriptor.node_kind)
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
pub(crate) fn refuse_port_names_not_cast_or_declared_twice(
    descriptor: &ProcessorDescriptor,
) -> Result<()> {
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
mod tests;

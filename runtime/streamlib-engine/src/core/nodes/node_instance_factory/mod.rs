// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

use parking_lot::RwLock;

use crate::core::NodeDescriptor;
use crate::core::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use crate::core::descriptors::{NodeClassImportPath, NodeKind};
use crate::core::descriptors::{
    PinConversion, PinConversionOutputPick, PinConversionSavedValueCast, PortDescriptor,
};
use crate::core::error::{Error, Result};
use crate::core::execution::ExecutionConfig;
use crate::core::graph::{GraphNode, GraphNodePorts, is_in_exposed_name_cast_form};
use crate::core::nodes::{Config, DynGeneratedNode, GeneratedNode};

pub struct NodeInstance(Box<dyn DynGeneratedNode + Send>);

impl NodeInstance {
    pub(crate) fn new(node: Box<dyn DynGeneratedNode + Send>) -> Self {
        Self(node)
    }

    pub fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.0.__generated_setup(ctx)
    }

    pub fn teardown(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.0.__generated_teardown(ctx)
    }

    pub fn on_pause(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        self.0.__generated_on_pause(ctx)
    }

    pub fn on_resume(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        self.0.__generated_on_resume(ctx)
    }

    pub fn process(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        self.0.process(ctx)
    }

    pub fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.0.start(ctx)
    }

    pub fn stop(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.0.stop(ctx)
    }

    pub fn execution_config(&self) -> ExecutionConfig {
        self.0.execution_config()
    }

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

    pub fn has_failed_unrecoverably(&self) -> bool {
        self.0.has_failed_unrecoverably()
    }

    pub fn failure_the_node_reported_of_itself(&self) -> Option<String> {
        self.0.failure_the_node_reported_of_itself()
    }

    pub fn why_process_is_raising_as_its_helper_reports(&self) -> Option<String> {
        self.0.why_process_is_raising_as_its_helper_reports()
    }

    pub fn why_the_node_is_not_ready_as_its_helper_reports(&self) -> Option<String> {
        self.0.why_the_node_is_not_ready_as_its_helper_reports()
    }

    pub fn why_the_out_of_process_helper_died_on_its_own(&self) -> Option<String> {
        self.0.why_the_out_of_process_helper_died_on_its_own()
    }

    pub(crate) fn ask_to_promote_the_function_node_that_lost_the_helper_that_died(
        &mut self,
        ctx: &RuntimeContextFullAccess<'_>,
    ) -> Option<super::FunctionNodePromotionAwaitedBeforeTheReaderRestarts> {
        self.0
            .ask_to_promote_the_function_node_that_lost_the_helper_that_died(ctx)
    }

    pub fn restart_the_out_of_process_helper_that_died(
        &mut self,
        ctx: &RuntimeContextFullAccess<'_>,
    ) -> Result<()> {
        self.0.restart_the_out_of_process_helper_that_died(ctx)
    }

    pub fn out_of_process_link_wiring(&self) -> Option<Arc<super::OutOfProcessLinkWiringEnvelope>> {
        self.0.out_of_process_link_wiring()
    }

    pub fn iceoryx2_output_writer_inner(&self) -> Option<Arc<crate::iceoryx2::OutputWriterInner>> {
        self.0.iceoryx2_output_writer_inner()
    }

    pub fn iceoryx2_input_mailboxes_inner(
        &self,
    ) -> Option<Arc<crate::iceoryx2::InputMailboxesInner>> {
        self.0.iceoryx2_input_mailboxes_inner()
    }

    pub fn install_iceoryx2_resources(&mut self) -> Result<()> {
        let needs_outputs = self.has_iceoryx2_outputs();
        let needs_inputs = self.has_iceoryx2_inputs();
        let output_inner =
            needs_outputs.then(|| Arc::new(crate::iceoryx2::OutputWriterInner::new()));
        if let (Some(output_inner), Some(descriptor)) = (&output_inner, self.0.descriptor()) {
            output_inner
                .declare_output_ports(descriptor.outputs.iter().map(|port| port.name.clone()));
            output_inner.keep_the_last_message_of_output_ports(
                descriptor
                    .outputs
                    .iter()
                    .filter(|port| port.flows_as_a_value())
                    .map(|port| port.name.clone()),
            );
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

    pub(crate) fn reach_the_stream_facts_of(&self, node: &GraphNode) {
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

    /// Hand the node `input_values`, the value each input reads while no link feeds it, where
    /// each input of `set_live` was just set live by a client.
    pub(crate) fn set_unlinked_input_values_naming_those_set_live(
        &mut self,
        input_values: &serde_json::Map<String, serde_json::Value>,
        set_live: &[String],
    ) -> Result<()> {
        self.0
            .set_unlinked_input_values_naming_those_set_live(input_values, set_live)
    }

    pub fn to_runtime_json(&self) -> serde_json::Value {
        self.0.to_runtime_json()
    }

    /// Downcast handle. Used by the host's compiler ops to reach
    /// host-only subprocess host wrappers.
    pub fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self.0.as_any_mut()
    }
}

pub type DynamicNodeConstructorFn =
    Box<dyn Fn(&GraphNode) -> Result<Box<dyn DynGeneratedNode + Send>> + Send + Sync>;

/// Refuse the settings a node holds, naming the node, its type and the setting.
type NodeSettingsTakenCheckFn = Box<
    dyn Fn(&str, &NodeClassImportPath, &serde_json::Map<String, serde_json::Value>) -> Result<()>
        + Send
        + Sync,
>;

/// What the factory stores for one host-compiled Rust type: its constructor, and for a type a
/// test registers with a config schema, the check its nodes' settings meet before any is added.
struct HostCompiledRustTypeRegistration {
    constructor: DynamicNodeConstructorFn,
    settings_taken_check: Option<NodeSettingsTakenCheckFn>,
}

pub struct NodeInstanceFactory {
    registrations: RwLock<HashMap<NodeClassImportPath, HostCompiledRustTypeRegistration>>,
    port_info: RwLock<HashMap<NodeClassImportPath, GraphNodePorts>>,
    descriptors: RwLock<HashMap<NodeClassImportPath, NodeDescriptor>>,
    /// The conversion each registered node type states, sorted by node type.
    conversions: RwLock<Arc<[PinConversion]>>,
    /// Each former built-in node type, with the module node that replaced it.
    built_in_node_types_moved_to_modules:
        RwLock<HashMap<NodeClassImportPath, ModuleNodeThatReplacedABuiltIn>>,
}

/// The module node type a former built-in became, and the distribution that
/// ships it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ModuleNodeThatReplacedABuiltIn {
    module_node_type: NodeClassImportPath,
    module_distribution: String,
}

pub static NODE_REGISTRY: LazyLock<NodeInstanceFactory> = LazyLock::new(NodeInstanceFactory::new);

impl Default for NodeInstanceFactory {
    fn default() -> Self {
        Self::new()
    }
}

/// Log, rather than return, `registered` when the registration of `P` failed.
/// `P`'s descriptor, refused naming `P` when it is not a `#[node]` type.
fn descriptor_of<P: GeneratedNode>() -> Result<NodeDescriptor> {
    <P as GeneratedNode>::descriptor().ok_or_else(|| {
        Error::Configuration(format!(
            "{} exposes no descriptor — it is not a #[node] type",
            std::any::type_name::<P>()
        ))
    })
}

fn warn_of_a_failed_registration_of<P>(registered: Result<NodeClassImportPath>) {
    if let Err(e) = registered {
        tracing::warn!(
            "Node type registration for {} failed: {}",
            std::any::type_name::<P>(),
            e
        );
    }
}

impl NodeInstanceFactory {
    pub fn new() -> Self {
        Self {
            registrations: RwLock::new(HashMap::new()),
            port_info: RwLock::new(HashMap::new()),
            descriptors: RwLock::new(HashMap::new()),
            conversions: RwLock::new(Arc::from([])),
            built_in_node_types_moved_to_modules: RwLock::new(HashMap::new()),
        }
    }

    /// Register a node type, storing `P`'s descriptor + port info, and
    /// log rather than return a registration that fails.
    pub fn register<P>(&self)
    where
        P: GeneratedNode + 'static,
        P::Config: Config,
    {
        warn_of_a_failed_registration_of::<P>(self.register_host_compiled_node_type::<P>());
    }

    /// Register `P`, a node type stating a conversion whose output follows a setting, with
    /// `pick`, the functions that pick that setting for the input its output feeds and give the
    /// type its output gives under the settings a node holds; log rather than return a
    /// registration that fails, or a `P` stating no conversion.
    pub fn register_a_conversion_whose_output_a_setting_picks<P>(
        &self,
        pick: PinConversionOutputPick,
    ) where
        P: GeneratedNode + 'static,
        P::Config: Config,
    {
        warn_of_a_failed_registration_of::<P>(
            self.register_host_compiled_node_type_supplying::<P>(WhatAConversionSupplies {
                pick: Some(pick),
                saved_value_cast: None,
            }),
        );
    }

    /// Register `P`, a node type stating a conversion, with `saved_value_cast`, the function that
    /// casts one saved value its input takes into the value its output gives with no stream
    /// started, as a retype of a saved value reads it; log rather than return a registration
    /// that fails, or a `P` stating no conversion.
    pub fn register_a_conversion_casting_a_saved_value<P>(
        &self,
        saved_value_cast: PinConversionSavedValueCast,
    ) where
        P: GeneratedNode + 'static,
        P::Config: Config,
    {
        warn_of_a_failed_registration_of::<P>(
            self.register_host_compiled_node_type_supplying::<P>(WhatAConversionSupplies {
                pick: None,
                saved_value_cast: Some(saved_value_cast),
            }),
        );
    }

    /// Register host-compiled Rust type `P` under its descriptor's import
    /// path, returned, with a constructor.
    pub(crate) fn register_host_compiled_node_type<P>(&self) -> Result<NodeClassImportPath>
    where
        P: GeneratedNode + 'static,
        P::Config: Config,
    {
        self.register_host_compiled_node_type_supplying::<P>(WhatAConversionSupplies::default())
    }

    /// Register `P` as [`Self::register_host_compiled_node_type`] does, the conversion it states
    /// carrying the functions `supplied` gives; any given to a `P` stating no conversion is
    /// refused.
    fn register_host_compiled_node_type_supplying<P>(
        &self,
        supplied: WhatAConversionSupplies,
    ) -> Result<NodeClassImportPath>
    where
        P: GeneratedNode + 'static,
        P::Config: Config,
    {
        self.register_host_compiled_node_type_under::<P>(descriptor_of::<P>()?, supplied, None)
    }

    /// Register `P` as [`Self::register`] does, its descriptor carrying `C`'s schema as its
    /// config schema: each field it lists is a setting of the type, which `graph` renders under
    /// a node's `config`, and a value set on one is checked by the field's schema. A node of it
    /// is refused before any is added unless `C` takes the settings it holds over `C`'s default,
    /// so a test can act on every load of a stream naming one. No node type declares a config
    /// schema of its own, so this is the node type a test of what a client still reads of one
    /// registers.
    #[cfg(any(test, feature = "test-support"))]
    pub fn register_with_a_config_schema_for_a_test<P, C>(&self)
    where
        P: GeneratedNode + 'static,
        P::Config: Config,
        C: Config + crate::core::descriptors::NodeConfigJsonSchema,
    {
        let config_schema = C::node_config_schema_document();
        let settings_the_schema_lists: Vec<String> = config_schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .map(|properties| properties.keys().cloned().collect())
            .unwrap_or_default();
        let settings_taken_check: NodeSettingsTakenCheckFn =
            Box::new(move |node_name, node_type, input_values| {
                let mut settings = match serde_json::to_value(C::default()) {
                    Ok(serde_json::Value::Object(defaults)) => defaults,
                    _ => serde_json::Map::new(),
                };
                settings.extend(
                    input_values
                        .iter()
                        .filter(|(name, _)| settings_the_schema_lists.contains(name))
                        .map(|(name, value)| (name.clone(), value.clone())),
                );
                serde_path_to_error::deserialize::<_, C>(serde_json::Value::Object(settings))
                    .map(drop)
                    .map_err(|refusal| Error::NodeConfigRefused {
                        node_name: node_name.to_string(),
                        node_type: node_type.clone(),
                        refusal: refusal.to_string(),
                    })
            });
        warn_of_a_failed_registration_of::<P>(descriptor_of::<P>().and_then(|descriptor| {
            self.register_host_compiled_node_type_under::<P>(
                descriptor.with_config_schema(config_schema),
                WhatAConversionSupplies::default(),
                Some(settings_taken_check),
            )
        }));
    }

    /// Register `P` under `descriptor`, with a constructor building each node of it from the
    /// default of its `Config`, which holds nothing: a node reads its settings off its input
    /// values. A node's settings meet `settings_taken_check`, when given, before it is added.
    fn register_host_compiled_node_type_under<P>(
        &self,
        descriptor: NodeDescriptor,
        supplied: WhatAConversionSupplies,
        settings_taken_check: Option<NodeSettingsTakenCheckFn>,
    ) -> Result<NodeClassImportPath>
    where
        P: GeneratedNode + 'static,
        P::Config: Config,
    {
        let constructor: DynamicNodeConstructorFn = Box::new(
            move |_node: &GraphNode| -> Result<Box<dyn DynGeneratedNode + Send>> {
                Ok(Box::new(P::from_config(P::Config::default())?))
            },
        );
        let node_class_import_path = descriptor.node_class_import_path.clone();
        self.register_with_constructor(
            descriptor,
            HostCompiledRustTypeRegistration {
                constructor,
                settings_taken_check,
            },
            supplied,
        )?;
        Ok(node_class_import_path)
    }

    /// Register `registration` under `descriptor`'s import path, refusing a
    /// path already claimed, the conversion it states carrying the functions `supplied` gives.
    fn register_with_constructor(
        &self,
        descriptor: NodeDescriptor,
        registration: HostCompiledRustTypeRegistration,
        supplied: WhatAConversionSupplies,
    ) -> Result<()> {
        let NodeTypeAsRegistered {
            descriptor,
            mut conversion,
        } = the_node_type_as_registered(descriptor)?;
        if supplied.pick.is_some() || supplied.saved_value_cast.is_some() {
            let Some(conversion) = conversion.as_mut() else {
                return Err(Error::Configuration(format!(
                    "`{}` is given a function of its conversion, and states no conversion",
                    descriptor.node_class_import_path
                )));
            };
            conversion.output_picked_for_the_input = supplied.pick;
            conversion.saved_value_cast = supplied.saved_value_cast;
        }
        let node_class_import_path = descriptor.node_class_import_path.clone();

        let ports = GraphNodePorts::of_a_node_just_placed(&descriptor);

        // The `descriptors` guard spans the check and the claim, so two threads
        // registering one path cannot both pass it. Checked against
        // `descriptors` rather than `registrations` because both registration
        // paths write it, and reading the narrower map would let a
        // constructor-bearing registration displace a descriptor-only one.
        let mut descriptors = self.descriptors.write();
        if descriptors.contains_key(&node_class_import_path) {
            return Err(duplicate_class_import_path(&node_class_import_path));
        }

        self.port_info
            .write()
            .insert(node_class_import_path.clone(), ports);

        self.registrations
            .write()
            .insert(node_class_import_path.clone(), registration);

        descriptors.insert(node_class_import_path.clone(), descriptor);
        self.store_the_conversion_stated(conversion);
        drop(descriptors);

        tracing::info!(
            node_class_import_path = node_class_import_path.as_str(),
            "new node type registered"
        );

        Ok(())
    }

    /// Register `descriptor` with no constructor; a conversion it states is never offered, since
    /// no node of it can be created.
    pub fn register_descriptor_only(&self, descriptor: NodeDescriptor) -> Result<()> {
        let NodeTypeAsRegistered { descriptor, .. } = the_node_type_as_registered(descriptor)?;
        let node_class_import_path = descriptor.node_class_import_path.clone();

        let ports = GraphNodePorts::of_a_node_just_placed(&descriptor);

        // One guard across the check and the claim — see
        // `register_with_constructor`.
        let mut descriptors = self.descriptors.write();
        if descriptors.contains_key(&node_class_import_path) {
            return Err(duplicate_class_import_path(&node_class_import_path));
        }

        self.port_info
            .write()
            .insert(node_class_import_path.clone(), ports);

        descriptors.insert(node_class_import_path.clone(), descriptor);
        drop(descriptors);

        tracing::info!(
            "[register_descriptor_only] node type registered without a constructor '{}'",
            node_class_import_path
        );

        Ok(())
    }

    /// Register `descriptor` with `constructor`.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn register_with_a_constructor_for_a_test(
        &self,
        descriptor: NodeDescriptor,
        constructor: DynamicNodeConstructorFn,
    ) -> Result<()> {
        self.register_with_constructor(
            descriptor,
            HostCompiledRustTypeRegistration {
                constructor,
                settings_taken_check: None,
            },
            WhatAConversionSupplies::default(),
        )
    }

    /// Register `descriptor`, stating a conversion whose output follows a setting, with
    /// `constructor` and `pick`.
    #[cfg(test)]
    pub(crate) fn register_a_conversion_picking_for_a_test(
        &self,
        descriptor: NodeDescriptor,
        constructor: DynamicNodeConstructorFn,
        pick: PinConversionOutputPick,
    ) -> Result<()> {
        self.register_with_constructor(
            descriptor,
            HostCompiledRustTypeRegistration {
                constructor,
                settings_taken_check: None,
            },
            WhatAConversionSupplies {
                pick: Some(pick),
                saved_value_cast: None,
            },
        )
    }

    /// Record a built-in that left the runtime for a module, so a graph naming
    /// it is refused naming the module's node type and the distribution to
    /// install.
    pub fn register_built_in_node_type_moved_to_a_module(
        &self,
        node_type: NodeClassImportPath,
        module_node_type: NodeClassImportPath,
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
        node_type: &NodeClassImportPath,
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

    /// Refuse a node this runtime cannot add before anything is added: a built-in this runtime
    /// lacks, or, on a type registered with a constructor, settings its check refuses or a value
    /// `input_values` sets on a name that is none of its inputs and no setting its config schema
    /// lists, naming the node, its type and the name. A type registered by its descriptor alone, a Python class, is checked
    /// where its node runs.
    pub fn refuse_a_node_this_runtime_cannot_add(
        &self,
        node_name: &str,
        node_type: &NodeClassImportPath,
        input_values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<()> {
        self.refuse_a_built_in_node_type_this_runtime_lacks(node_type)?;
        match self.registrations.read().get(node_type) {
            None => return Ok(()),
            Some(registration) => {
                if let Some(settings_taken_check) = &registration.settings_taken_check {
                    settings_taken_check(node_name, node_type, input_values)?;
                }
            }
        }
        let (Some(ports), Some(descriptor)) =
            (self.port_info(node_type), self.descriptor(node_type))
        else {
            return Ok(());
        };
        let settings_its_schema_lists = crate::core::nodes::the_settings_a_config_schema_lists(
            descriptor.config_schema.as_ref(),
        );
        let Some(name_it_does_not_take) = input_values.keys().find(|name| {
            ports.the_input_of_the_type_a_name_finds(name).is_none()
                && !settings_its_schema_lists.contains(name.as_str())
        }) else {
            return Ok(());
        };
        let mut what_it_takes: Vec<String> = ports
            .inputs
            .iter()
            .map(|input| format!("`{}`", input.name))
            .chain(
                settings_its_schema_lists
                    .iter()
                    .map(|name| format!("`{name}`")),
            )
            .collect();
        what_it_takes.sort();
        Err(Error::NodeConfigRefused {
            node_name: node_name.to_string(),
            node_type: node_type.clone(),
            refusal: format!(
                "unknown input `{name_it_does_not_take}`, expected {}",
                if what_it_takes.is_empty() {
                    "none".to_owned()
                } else {
                    what_it_takes.join(", ")
                }
            ),
        })
    }

    pub fn can_create(&self, node_type: &NodeClassImportPath) -> bool {
        self.registrations.read().contains_key(node_type)
    }

    pub fn create(&self, node: &GraphNode) -> Result<NodeInstance> {
        let registrations = self.registrations.read();
        let registration = registrations.get(&node.node_type).ok_or_else(|| {
            Error::NodeNotFound(format!(
                "No factory registered for node type '{}'",
                node.node_type
            ))
        })?;

        let mut instance = NodeInstance::new((registration.constructor)(node)?);
        instance.install_iceoryx2_resources()?;
        instance.reach_the_stream_facts_of(node);
        Ok(instance)
    }

    pub fn port_info(&self, node_type: &NodeClassImportPath) -> Option<GraphNodePorts> {
        self.port_info.read().get(node_type).cloned()
    }

    pub fn is_registered(&self, node_type: &NodeClassImportPath) -> bool {
        self.registrations.read().contains_key(node_type)
    }

    pub fn descriptor(&self, node_type: &NodeClassImportPath) -> Option<NodeDescriptor> {
        self.descriptors.read().get(node_type).cloned()
    }

    pub(crate) fn node_kind(&self, node_type: &NodeClassImportPath) -> Option<NodeKind> {
        self.descriptors
            .read()
            .get(node_type)
            .map(|descriptor| descriptor.node_kind)
    }

    /// The class's short name, as the registering surface declared it — what
    /// an instance's display name defaults to.
    ///
    /// Projects the one field out under the read lock rather than going
    /// through [`Self::descriptor`], which clones the whole descriptor: the
    /// snapshot path asks this once per node.
    pub(crate) fn default_display_name(&self, node_type: &NodeClassImportPath) -> Option<String> {
        self.descriptors
            .read()
            .get(node_type)
            .map(|descriptor| descriptor.node_class_short_name.as_str().to_string())
    }

    pub fn registered_node_class_import_paths(&self) -> Vec<NodeClassImportPath> {
        self.descriptors.read().keys().cloned().collect()
    }

    pub fn list_registered(&self) -> Vec<NodeDescriptor> {
        self.descriptors.read().values().cloned().collect()
    }

    /// The conversion each node type registered with a constructor states, which the engine
    /// offers, sorted by node type; a type registered by its descriptor alone offers none.
    pub fn pin_conversions(&self) -> Arc<[PinConversion]> {
        Arc::clone(&self.conversions.read())
    }

    /// Store `conversion` beside the others, kept sorted; called under the `descriptors` guard.
    fn store_the_conversion_stated(&self, conversion: Option<PinConversion>) {
        let Some(conversion) = conversion else {
            return;
        };
        let mut conversions = self.conversions.write();
        let mut stored = conversions.to_vec();
        stored.push(conversion);
        *conversions = in_node_type_order(stored);
    }
}

/// The functions a host-compiled conversion is registered with beside its descriptor.
#[derive(Default)]
struct WhatAConversionSupplies {
    /// Picks the setting its output follows for the input it feeds.
    pick: Option<PinConversionOutputPick>,
    /// Casts one saved value with no stream started.
    saved_value_cast: Option<PinConversionSavedValueCast>,
}

/// `conversions` sorted by the node type stating each, as every list of them is kept.
pub(crate) fn in_node_type_order(mut conversions: Vec<PinConversion>) -> Arc<[PinConversion]> {
    conversions.sort_by(|one, other| one.node_type.cmp(&other.node_type));
    conversions.into()
}

fn duplicate_class_import_path(node_class_import_path: &NodeClassImportPath) -> Error {
    Error::Configuration(format!(
        "two nodes both identify as `{node_class_import_path}`, and one import path \
         names one class. In Python this means the module was loaded twice, so this class object \
         is not the one the registry holds — `importlib.reload` is the usual cause. In Rust it \
         means two `#[node]` types share a module path, which happens when one is \
         declared inside a function body: a function's name is not part of a module path, so \
         neither type is reachable by `use`. Declare nodes at module scope."
    ))
}

/// `descriptor` as its node type is registered: each input with a pin that
/// states no delivery profile reads by its pin's default. Refuses a port not in
/// cast form under a name of its own — every lookup casts its argument, so a
/// port registered under an uncast name could never be reached — a name a port
/// had that would not find that port alone, an output that may be unset, a flow
/// a port's pin type does not take, a config field named as an input port, and
/// a conversion of any shape but one wired input and one output, each typed.
pub(crate) fn the_node_type_as_registered(
    descriptor: NodeDescriptor,
) -> Result<NodeTypeAsRegistered> {
    refuse_a_pin_group_no_node_type_may_declare(&descriptor)?;
    let with_one_slot_in_each_pin_group = descriptor.with_one_slot_in_each_pin_group();
    refuse_port_names_not_cast_or_declared_twice(&with_one_slot_in_each_pin_group)?;
    refuse_former_port_names_that_do_not_find_one_port(&with_one_slot_in_each_pin_group)?;
    refuse_an_output_that_may_be_unset(&with_one_slot_in_each_pin_group)?;
    let conversion = descriptor
        .pin_conversion()
        .map_err(|refusal| Error::Configuration(refusal.to_string()))?;
    crate::core::nodes::refuse_a_config_field_named_as_an_input_port(&descriptor)?;
    let descriptor = descriptor
        .with_input_deliveries_defaulted_by_pin()
        .map_err(Error::Configuration)?;
    Ok(NodeTypeAsRegistered {
        descriptor,
        conversion,
    })
}

/// Refuse a pin group the node type may not declare, naming it: one
/// [`crate::core::descriptors::PinGroupDescriptor::refusal_of_its_declaration`] refuses, one
/// named as another group or as a port the type declares, and one a port of the type is named
/// as a port of a slot of.
fn refuse_a_pin_group_no_node_type_may_declare(descriptor: &NodeDescriptor) -> Result<()> {
    let refused = |why: String| {
        Err(Error::Configuration(format!(
            "`{}`: {why}",
            descriptor.node_class_import_path
        )))
    };
    let mut group_names = HashSet::new();
    for group in &descriptor.pin_groups {
        if let Some(why) = group.refusal_of_its_declaration() {
            return refused(why);
        }
        if !group_names.insert(group.name.as_str())
            || descriptor
                .inputs
                .iter()
                .chain(&descriptor.outputs)
                .any(|port| port.name == group.name || group.holds_the_slot_port(&port.name))
        {
            return refused(format!(
                "pin group `{}` is named as another pin group or a port of the type, or a port \
                 of the type is named as a port of one of its slots",
                group.name
            ));
        }
    }
    Ok(())
}

/// A node type as it is registered, with the conversion it states.
pub(crate) struct NodeTypeAsRegistered {
    /// Its descriptor.
    pub(crate) descriptor: NodeDescriptor,
    /// The conversion it states, if it states one.
    pub(crate) conversion: Option<PinConversion>,
}

/// Whether `output` publishes the setting `input` of its name: `input` is a setting, and both
/// declare one pin type.
fn publishes_the_setting_of_its_name(input: &PortDescriptor, output: &PortDescriptor) -> bool {
    let (Some(input_pin), Some(output_pin)) = (&input.pin_declaration, &output.pin_declaration)
    else {
        return false;
    };
    input.is_a_setting() && input_pin.pin_type == output_pin.pin_type
}

/// Refuse a port name not in cast form, and one declared twice: an output takes the name of an
/// input only when that input is the setting whose value the output publishes, both of one
/// pin type, as a value source's and a view node's are.
fn refuse_port_names_not_cast_or_declared_twice(descriptor: &NodeDescriptor) -> Result<()> {
    let declared_twice = |port: &PortDescriptor| Error::DescriptorPortNameDeclaredTwice {
        node_class_import_path: descriptor.node_class_import_path.clone(),
        port_name: port.name.clone(),
    };
    let mut input_names_seen = HashMap::new();
    let mut output_names_seen = HashSet::new();
    for port in descriptor.inputs.iter().chain(&descriptor.outputs) {
        if !is_in_exposed_name_cast_form(&port.name) {
            return Err(Error::DescriptorPortNameNotCast {
                node_class_import_path: descriptor.node_class_import_path.clone(),
                port_name: port.name.clone(),
            });
        }
    }
    for input in &descriptor.inputs {
        if input_names_seen
            .insert(input.name.as_str(), input)
            .is_some()
        {
            return Err(declared_twice(input));
        }
    }
    for output in &descriptor.outputs {
        let shares_an_input_name = input_names_seen
            .get(output.name.as_str())
            .is_some_and(|input| !publishes_the_setting_of_its_name(input, output));
        if !output_names_seen.insert(output.name.as_str()) || shares_an_input_name {
            return Err(declared_twice(output));
        }
    }
    Ok(())
}

/// Refuse a name a port of `descriptor` had that would not find that port alone: one not in
/// cast form, one that is the current name of a port of the type, and one two of its ports list.
fn refuse_former_port_names_that_do_not_find_one_port(descriptor: &NodeDescriptor) -> Result<()> {
    let node_type = &descriptor.node_class_import_path;
    let ports: Vec<_> = descriptor
        .inputs
        .iter()
        .chain(&descriptor.outputs)
        .collect();
    let mut port_of_each_former_name: HashMap<String, &str> = HashMap::new();
    for port in &ports {
        let Some(declared) = port.declared_pin() else {
            continue;
        };
        for former_name in declared.former_names {
            let refusal = if !is_in_exposed_name_cast_form(&former_name) {
                Some(format!(
                    "port `{}` lists `{former_name}` as a name it had, which is not in the cast \
                     form every port name takes",
                    port.name
                ))
            } else if let Some(holder) = ports.iter().find(|held| held.name == former_name) {
                Some(format!(
                    "port `{}` lists `{former_name}` as a name it had, and port `{}` is named \
                     that now",
                    port.name, holder.name
                ))
            } else if let Some(other) = port_of_each_former_name.get(&former_name) {
                Some(format!(
                    "ports `{other}` and `{}` both list `{former_name}` as a name they had",
                    port.name
                ))
            } else {
                port_of_each_former_name.insert(former_name.clone(), &port.name);
                None
            };
            if let Some(refusal) = refusal {
                return Err(Error::Configuration(format!(
                    "`{node_type}`: {refusal}, so the name would not find one port"
                )));
            }
        }
    }
    Ok(())
}

/// Refuse an output of `descriptor` that declares it may be unset, which only an input may.
fn refuse_an_output_that_may_be_unset(descriptor: &NodeDescriptor) -> Result<()> {
    match descriptor.outputs.iter().find(|output| {
        output
            .declared_pin()
            .is_some_and(|declared| declared.may_be_unset)
    }) {
        Some(output) => Err(Error::Configuration(format!(
            "`{}`: output `{}` declares it may be unset, which only an input may: an output \
             holds no value",
            descriptor.node_class_import_path, output.name
        ))),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests;

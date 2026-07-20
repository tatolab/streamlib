// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use super::Runner;
use super::operations::{BoxFuture, ConnectOptions, RuntimeOperations};
use super::runtime::TokioRuntimeVariant;
use crate::core::compiler::{Compiler, PendingOperation};
use crate::core::graph::{
    GraphEdgeWithComponents, GraphNodeWithComponents, LinkUniqueId, PendingDeletionComponent,
    ProcessorUniqueId, StateComponent,
};
use crate::core::embedded_schemas::resolve_node_port_schema;
use crate::core::processors::{ProcessorSpec, ProcessorState};
use crate::core::pubsub::{Event, PUBSUB, RuntimeEvent, topics};
use crate::core::schema_agreement::{
    ConnectSchemaContext, SchemaValidationPosture, enforce_connect_schema_agreement,
};
use crate::core::{Error, InputLinkPortRef, OutputLinkPortRef, PortDirection, Result};
use streamlib_idents::ChannelName;

// =============================================================================
// Core Implementation Functions ('static async fns for spawn compatibility)
// =============================================================================

/// Core implementation for add_processor - takes owned Arcs for 'static lifetime.
///
/// `lazy_error` carries the outcome of the lazy plugin-discovery step the
/// caller ran before this (see [`Runner::lazily_load_provider_for_processor_type`]):
/// `Some` when discovery was ambiguous or a discovered package failed to load.
/// On a registry miss it is returned in place of the generic
/// [`Error::UnknownProcessorType`], so the app sees the specific recoverable
/// reason while the failed node is still surfaced in the graph for
/// observability. `None` (type available after the lazy load, or no package
/// provided it) leaves the existing behavior unchanged.
async fn add_processor_impl(
    compiler: Arc<Compiler>,
    spec: ProcessorSpec,
    lazy_error: Option<Error>,
) -> Result<ProcessorUniqueId> {
    let emit_will_add = |id: &ProcessorUniqueId| {
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeWillAddProcessor {
                processor_id: id.clone(),
            }),
        );
    };

    let emit_did_add = |id: &ProcessorUniqueId| {
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeDidAddProcessor {
                processor_id: id.clone(),
            }),
        );
    };

    // Hold a diagnostic ident so we can surface a typed `UnknownProcessorType`
    // error if the registry doesn't know this type — `spec` is moved into
    // `add_v`. A version-free reference projects to `(org, package, type)@0.0.0`.
    let ident_for_err = spec.name.to_diagnostic_ident();

    let processor_id = compiler.scope(|graph, tx| -> Result<ProcessorUniqueId> {
        let node_id = graph
            .traversal_mut()
            .add_v(spec)
            .first()
            .map(|node| node.id.clone())
            .ok_or_else(|| Error::GraphError("Could not create node".into()))?;

        // Registry miss: `add_v` already attached `StateComponent(Error)` so
        // the failed node is visible via `GET /api/graph`. Skip pending-op
        // logging so the compiler doesn't try to spawn it. Emit the
        // graph-changed events so subscribers see the new node, then surface
        // the typed error.
        let registry_miss = graph
            .traversal()
            .v(&node_id)
            .first()
            .and_then(|node| node.get::<StateComponent>())
            .map(|state_component| matches!(*state_component.0.lock(), ProcessorState::Error))
            .unwrap_or(false);

        if registry_miss {
            emit_will_add(&node_id);
            emit_did_add(&node_id);
            // A specific lazy-discovery failure (ambiguous providers, or a
            // discovered package that failed to load) supersedes the generic
            // unknown-type error; a plain "no package provides it" leaves
            // `lazy_error` None and falls back to UnknownProcessorType.
            return Err(lazy_error.unwrap_or(Error::UnknownProcessorType {
                ident: ident_for_err,
            }));
        }

        emit_will_add(&node_id);
        tx.log(PendingOperation::AddProcessor(node_id.clone()));
        emit_did_add(&node_id);
        Ok(node_id)
    })?;

    PUBSUB.publish(
        topics::RUNTIME_GLOBAL,
        &Event::RuntimeGlobal(RuntimeEvent::GraphDidChange),
    );

    Ok(processor_id)
}

/// Core implementation for remove_processor - takes owned Arcs for 'static lifetime.
async fn remove_processor_impl(
    compiler: Arc<Compiler>,
    processor_id: ProcessorUniqueId,
) -> Result<()> {
    compiler.scope(|graph, tx| {
        if !graph.traversal().v(&processor_id).exists() {
            return Err(Error::ProcessorNotFound(processor_id.to_string()));
        }

        if let Some(node) = graph.traversal_mut().v(&processor_id).first_mut() {
            node.insert(PendingDeletionComponent);
        }

        tx.log(PendingOperation::RemoveProcessor(processor_id.clone()));

        Ok(())
    })?;

    PUBSUB.publish(
        topics::RUNTIME_GLOBAL,
        &Event::RuntimeGlobal(RuntimeEvent::RuntimeWillRemoveProcessor {
            processor_id: processor_id.clone(),
        }),
    );

    PUBSUB.publish(
        topics::RUNTIME_GLOBAL,
        &Event::RuntimeGlobal(RuntimeEvent::RuntimeDidRemoveProcessor {
            processor_id: processor_id.clone(),
        }),
    );

    PUBSUB.publish(
        topics::RUNTIME_GLOBAL,
        &Event::RuntimeGlobal(RuntimeEvent::GraphDidChange),
    );

    Ok(())
}

/// Core implementation for connect - takes owned Arcs for 'static lifetime.
///
/// `validation` selects the schema-agreement posture for this wiring site.
/// [`connect`](Runner::connect) / [`connect_async`](RuntimeOperations::connect_async)
/// pass [`SchemaValidationPosture::Loose`] (warn-but-wire); the
/// [`connect_with`](Runner::connect_with) opt-in passes the caller's
/// [`ConnectOptions`] posture, so a safety-critical channel selects
/// [`Strict`][SchemaValidationPosture::Strict] to hard-fail a concrete
/// producer/consumer schema mismatch with [`Error::SchemaIdentMismatch`]
/// instead of only warning.
#[tracing::instrument(
    name = "runtime.connect",
    skip(compiler),
    fields(from = %from, to = %to, validation = ?validation),
)]
async fn connect_impl(
    compiler: Arc<Compiler>,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
    validation: SchemaValidationPosture,
) -> Result<LinkUniqueId> {
    let from_processor = from.processor_id.clone();
    let from_port = from.port_name.clone();
    let to_processor = to.processor_id.clone();
    let to_port = to.port_name.clone();

    PUBSUB.publish(
        topics::RUNTIME_GLOBAL,
        &Event::RuntimeGlobal(RuntimeEvent::RuntimeWillConnect {
            from_processor: from_processor.clone(),
            from_port: from_port.clone(),
            to_processor: to_processor.clone(),
            to_port: to_port.clone(),
        }),
    );

    let (link_id, channel) = compiler.scope(|graph, tx| -> Result<(LinkUniqueId, ChannelName)> {
        // Validate endpoints + ports FIRST — before the channel-name
        // derivation — so a missing processor/port reads as the typed
        // ProcessorNotFound / ProcessorPortNotFound and never gets masked by an
        // InvalidLink from the wire-name grammar. The `add_e` call still checks
        // defensively; this pre-validation is what gets the typed error out.
        // Validate source processor + output port.
        {
            let from_node = graph
                .traversal()
                .v(&from.processor_id)
                .first()
                .ok_or_else(|| Error::ProcessorNotFound(from.processor_id.to_string()))?;
            if !from_node.has_output(&from.port_name) {
                return Err(Error::ProcessorPortNotFound {
                    processor_id: from.processor_id.to_string(),
                    port_name: from.port_name.clone(),
                    direction: PortDirection::Output,
                });
            }
        }
        // Validate target processor + input port.
        {
            let to_node = graph
                .traversal()
                .v(&to.processor_id)
                .first()
                .ok_or_else(|| Error::ProcessorNotFound(to.processor_id.to_string()))?;
            if !to_node.has_input(&to.port_name) {
                return Err(Error::ProcessorPortNotFound {
                    processor_id: to.processor_id.to_string(),
                    port_name: to.port_name.clone(),
                    direction: PortDirection::Input,
                });
            }
        }

        // Schema-agreement check at the wiring site: resolve the producer's
        // output schema and the consumer's input schema from the registry and
        // compare. A wildcard (`any`) on either side never mismatches; two
        // concrete-but-unequal schemas warn (loose) or hard-fail (strict).
        // Runs before `add_e` so a strict rejection rolls the pending link
        // back rather than committing a mismatched edge. Endpoints are already
        // validated to exist above.
        {
            let producer_schema = resolve_node_port_schema(
                graph,
                &from.processor_id,
                &from.port_name,
                PortDirection::Output,
            );
            let consumer_schema = resolve_node_port_schema(
                graph,
                &to.processor_id,
                &to.port_name,
                PortDirection::Input,
            );
            enforce_connect_schema_agreement(
                &producer_schema,
                &consumer_schema,
                validation,
                ConnectSchemaContext {
                    from_processor: from.processor_id.as_str(),
                    from_port: &from.port_name,
                    to_processor: to.processor_id.as_str(),
                    to_port: &to.port_name,
                },
            )?;
        }

        // The one channel this link's source output port publishes to — keyed
        // on the SOURCE only (`{src_processor}/{src_output}`), so every link
        // from this output port shares one channel / one publisher / N
        // subscribers (D1, #1419). Endpoints are validated above, so a grammar
        // failure here is a genuinely-illegal source PORT name (author error),
        // surfaced as InvalidLink. The processor id is lowercased inside
        // `source_channel_name`; underscore is legal and rides through. Deriving
        // inside the transaction means an illegal port name rolls the pending
        // link back rather than committing a half-built edge.
        let channel = streamlib_idents::source_channel_name(
            from.processor_id.as_str(),
            &from.port_name,
        )
        .map_err(|source| Error::InvalidLink(source.to_string()))?;

        let link_id = graph
            .traversal_mut()
            .add_e(from, to)
            .inspect(|link| tx.log(PendingOperation::AddLink(link.id.clone())))
            .first()
            .map(|link| link.id.clone())
            .ok_or_else(|| Error::GraphError("failed to create link after validation".into()))?;
        Ok((link_id, channel))
    })?;

    tracing::debug!(
        link_id = %link_id,
        channel = channel.as_str(),
        "connect assigned channel"
    );

    PUBSUB.publish(
        topics::RUNTIME_GLOBAL,
        &Event::RuntimeGlobal(RuntimeEvent::RuntimeDidConnect {
            link_id: link_id.to_string(),
            from_port,
            to_port,
        }),
    );

    PUBSUB.publish(
        topics::RUNTIME_GLOBAL,
        &Event::RuntimeGlobal(RuntimeEvent::GraphDidChange),
    );

    Ok(link_id)
}

/// Core implementation for disconnect - takes owned Arcs for 'static lifetime.
async fn disconnect_impl(compiler: Arc<Compiler>, link_id: LinkUniqueId) -> Result<()> {
    let link_info = compiler.scope(|graph, tx| {
        let (from_value, to_value) = graph
            .traversal()
            .e(&link_id)
            .first()
            .map(|l| (l.from_port(), l.to_port()))
            .ok_or_else(|| Error::NotFound(format!("Link '{}' not found", link_id)))?;

        let info = (
            OutputLinkPortRef::new(from_value.processor_id.clone(), to_value.port_name.clone()),
            InputLinkPortRef::new(to_value.processor_id.clone(), to_value.port_name.clone()),
        );

        if let Some(link) = graph.traversal_mut().e(&link_id).first_mut() {
            link.insert(PendingDeletionComponent);
        }

        tx.log(PendingOperation::RemoveLink(link_id.clone()));

        Ok::<_, Error>(info)
    })?;

    PUBSUB.publish(
        topics::RUNTIME_GLOBAL,
        &Event::RuntimeGlobal(RuntimeEvent::RuntimeWillDisconnect {
            link_id: link_id.to_string(),
            from_port: link_info.0.to_string(),
            to_port: link_info.1.to_string(),
        }),
    );

    PUBSUB.publish(
        topics::RUNTIME_GLOBAL,
        &Event::RuntimeGlobal(RuntimeEvent::RuntimeDidDisconnect {
            link_id: link_id.to_string(),
            from_port: link_info.0.to_string(),
            to_port: link_info.1.to_string(),
        }),
    );

    PUBSUB.publish(
        topics::RUNTIME_GLOBAL,
        &Event::RuntimeGlobal(RuntimeEvent::GraphDidChange),
    );

    Ok(())
}

// =============================================================================
// RuntimeOperations Implementation
// =============================================================================

impl RuntimeOperations for Runner {
    // =========================================================================
    // Async Methods (delegate to _impl functions)
    // =========================================================================

    fn add_processor_async(&self, spec: ProcessorSpec) -> BoxFuture<'_, Result<ProcessorUniqueId>> {
        let compiler = Arc::clone(&self.compiler);
        Box::pin(async move {
            // Lazy plugin auto-discovery: if the referenced processor type
            // isn't registered, discover + load the providing package from
            // streamlib_modules/ on first reference. A recoverable failure is
            // threaded into add_processor_impl (the runtime keeps running).
            let lazy_error = self
                .lazily_load_provider_for_processor_type(&spec.name)
                .await;
            add_processor_impl(compiler, spec, lazy_error).await
        })
    }

    fn remove_processor_async(&self, processor_id: ProcessorUniqueId) -> BoxFuture<'_, Result<()>> {
        let compiler = Arc::clone(&self.compiler);
        Box::pin(remove_processor_impl(compiler, processor_id))
    }

    fn connect_async(
        &self,
        from: OutputLinkPortRef,
        to: InputLinkPortRef,
    ) -> BoxFuture<'_, Result<LinkUniqueId>> {
        self.connect_with_async(from, to, ConnectOptions::loose())
    }

    fn disconnect_async(&self, link_id: LinkUniqueId) -> BoxFuture<'_, Result<()>> {
        let compiler = Arc::clone(&self.compiler);
        Box::pin(disconnect_impl(compiler, link_id))
    }

    fn to_json_async(&self) -> BoxFuture<'_, Result<serde_json::Value>> {
        Box::pin(async move { Runner::to_json(self) })
    }

    // =========================================================================
    // Sync Methods (variant-aware blocking strategy)
    // =========================================================================

    fn add_processor(&self, spec: ProcessorSpec) -> Result<ProcessorUniqueId> {
        match &self.tokio_runtime_variant {
            TokioRuntimeVariant::OwnedTokioRuntime(rt) => {
                // The async path runs the lazy plugin-discovery step itself.
                rt.block_on(self.add_processor_async(spec))
            }
            TokioRuntimeVariant::ExternalTokioHandle(handle) => {
                // Can't `.await` the borrowing lazy-load future in the spawned
                // 'static task, so drive lazy discovery to completion here
                // (blocking) and hand the outcome to the owned add_processor_impl.
                let lazy_error =
                    self.lazily_load_provider_for_processor_type_blocking(&spec.name);
                let compiler = Arc::clone(&self.compiler);
                let (tx, rx) = std::sync::mpsc::channel();
                handle.spawn(async move {
                    let result = add_processor_impl(compiler, spec, lazy_error).await;
                    let _ = tx.send(result);
                });
                rx.recv()
                    .map_err(|_| Error::Runtime("Task channel closed".into()))?
            }
        }
    }

    fn remove_processor(&self, processor_id: &ProcessorUniqueId) -> Result<()> {
        match &self.tokio_runtime_variant {
            TokioRuntimeVariant::OwnedTokioRuntime(rt) => {
                rt.block_on(self.remove_processor_async(processor_id.clone()))
            }
            TokioRuntimeVariant::ExternalTokioHandle(handle) => {
                let compiler = Arc::clone(&self.compiler);
                let processor_id = processor_id.clone();
                let (tx, rx) = std::sync::mpsc::channel();
                handle.spawn(async move {
                    let result = remove_processor_impl(compiler, processor_id).await;
                    let _ = tx.send(result);
                });
                rx.recv()
                    .map_err(|_| Error::Runtime("Task channel closed".into()))?
            }
        }
    }

    fn connect(&self, from: OutputLinkPortRef, to: InputLinkPortRef) -> Result<LinkUniqueId> {
        self.connect_with(from, to, ConnectOptions::loose())
    }

    fn disconnect(&self, link_id: &LinkUniqueId) -> Result<()> {
        match &self.tokio_runtime_variant {
            TokioRuntimeVariant::OwnedTokioRuntime(rt) => {
                rt.block_on(self.disconnect_async(link_id.clone()))
            }
            TokioRuntimeVariant::ExternalTokioHandle(handle) => {
                let compiler = Arc::clone(&self.compiler);
                let link_id = link_id.clone();
                let (tx, rx) = std::sync::mpsc::channel();
                handle.spawn(async move {
                    let result = disconnect_impl(compiler, link_id).await;
                    let _ = tx.send(result);
                });
                rx.recv()
                    .map_err(|_| Error::Runtime("Task channel closed".into()))?
            }
        }
    }

    // =========================================================================
    // Introspection
    // =========================================================================

    fn to_json(&self) -> Result<serde_json::Value> {
        Runner::to_json(self)
    }
}

impl Runner {
    /// Connect two ports under explicit [`ConnectOptions`] — the strict
    /// schema-validation opt-in for a safety-critical wiring site.
    ///
    /// [`connect`](RuntimeOperations::connect) is the loose-but-observed default
    /// (a concrete producer/consumer schema mismatch warns, then wires the link
    /// anyway); this threads the caller's posture into the same wiring path, so
    /// under [`ConnectOptions::strict`] the mismatch instead hard-fails with
    /// [`Error::SchemaIdentMismatch`] and the link is not wired.
    pub fn connect_with(
        &self,
        from: OutputLinkPortRef,
        to: InputLinkPortRef,
        options: ConnectOptions,
    ) -> Result<LinkUniqueId> {
        match &self.tokio_runtime_variant {
            TokioRuntimeVariant::OwnedTokioRuntime(rt) => rt.block_on(connect_impl(
                Arc::clone(&self.compiler),
                from,
                to,
                options.validation,
            )),
            TokioRuntimeVariant::ExternalTokioHandle(handle) => {
                let compiler = Arc::clone(&self.compiler);
                let (tx, rx) = std::sync::mpsc::channel();
                handle.spawn(async move {
                    let result = connect_impl(compiler, from, to, options.validation).await;
                    let _ = tx.send(result);
                });
                rx.recv()
                    .map_err(|_| Error::Runtime("Task channel closed".into()))?
            }
        }
    }

    /// Async form of [`connect_with`](Self::connect_with) — safe from any
    /// context, including a tokio task.
    pub fn connect_with_async(
        &self,
        from: OutputLinkPortRef,
        to: InputLinkPortRef,
        options: ConnectOptions,
    ) -> BoxFuture<'_, Result<LinkUniqueId>> {
        let compiler = Arc::clone(&self.compiler);
        Box::pin(connect_impl(compiler, from, to, options.validation))
    }
}

#[cfg(test)]
mod channel_wire_bound_tests {
    // The channel-name grammar (streamlib_idents) and the fixed PortKey wire
    // capacity (streamlib_ipc_types) live in separate crates that cannot depend
    // on each other, so the max-length bound is duplicated. This engine layer
    // depends on both and is the one place that reconciles them: a channel name
    // that passes the grammar must always fit the wire.
    #[test]
    fn channel_name_bound_matches_port_key_wire_capacity() {
        assert_eq!(
            streamlib_idents::MAX_CHANNEL_NAME_BYTES,
            streamlib_ipc_types::PortKey::MAX_NAME_BYTES,
            "channel-name grammar bound drifted from the PortKey wire capacity"
        );
    }

    #[test]
    fn generated_source_channel_name_fits_the_wire() {
        // A generated name — including the hash-legalized over-budget path —
        // must construct a PortKey without the fallible constructor rejecting it.
        let long = "verylongprocessorname".repeat(4);
        let channel = streamlib_idents::source_channel_name(&long, "outputport")
            .expect("a grammar-legal source output port must produce a channel name");
        streamlib_ipc_types::PortKey::new(channel.as_str())
            .expect("a grammar-legal channel name must always fit the PortKey wire");
    }
}

#[cfg(test)]
mod connect_schema_agreement_tests {
    //! End-to-end connect-path revert lock for the connect-time schema
    //! agreement check (#1430). Drives [`connect_impl`] against two registered
    //! processor types whose concrete output / input schemas disagree, and
    //! asserts the posture-dependent outcome: [`Loose`] warns but still wires
    //! the link; [`Strict`] rejects it with a typed [`Error::SchemaIdentMismatch`].
    //!
    //! Mentally reverting the connect-time `enforce_connect_schema_agreement`
    //! call in [`connect_impl`] collapses both halves — the Loose warn stops
    //! firing and Strict stops rejecting — failing this module. The unit tests
    //! on `enforce_connect_schema_agreement` alone do NOT catch that regression:
    //! they never exercise the wiring site.
    //!
    //! [`Loose`]: SchemaValidationPosture::Loose
    //! [`Strict`]: SchemaValidationPosture::Strict

    use std::sync::{Arc, Mutex, Once};

    use serde_json::Value;
    use tracing::field::{Field, Visit};
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::{Context, SubscriberExt};

    use serial_test::serial;

    use super::connect_impl;
    use super::{ConnectOptions, Runner};
    use crate::core::Error;
    use crate::core::compiler::Compiler;
    use crate::core::descriptors::{PortDescriptor, ProcessorDescriptor};
    use crate::core::graph::{InputLinkPortRef, OutputLinkPortRef, ProcessorUniqueId};
    use crate::core::processors::{PROCESSOR_REGISTRY, ProcessorSpec};
    use crate::core::schema_agreement::SchemaValidationPosture;
    use streamlib_idents::{Org, Package, SchemaIdent, SemVer, TypeName};
    use streamlib_processor_schema::PortSchemaSpec;

    const PRODUCER_TYPE: &str = "SchemaMismatchProducer";
    const CONSUMER_TYPE: &str = "SchemaMismatchConsumer";

    fn ident(package: &str, ty: &str) -> SchemaIdent {
        SchemaIdent::new(
            Org::new("test").unwrap(),
            Package::new(package).unwrap(),
            TypeName::new(ty).unwrap(),
            SemVer::new(1, 0, 0),
        )
    }

    fn schema(ty: &str) -> PortSchemaSpec {
        PortSchemaSpec::Specific(ident("core", ty))
    }

    /// Register a producer type (`out` → VideoFrame) and a consumer type
    /// (`in` → AudioFrame) so any wired producer→consumer link is a concrete
    /// schema mismatch. Idempotent across tests in the process.
    fn ensure_mismatch_types_registered() {
        static REGISTER: Once = Once::new();
        REGISTER.call_once(|| {
            let mut producer =
                ProcessorDescriptor::new(ident("connectcheck", PRODUCER_TYPE), "mismatch producer");
            producer
                .outputs
                .push(PortDescriptor::iceoryx2("out", "output", schema("VideoFrame")));
            PROCESSOR_REGISTRY
                .register_descriptor_only(producer)
                .expect("register mismatch producer descriptor");

            let mut consumer =
                ProcessorDescriptor::new(ident("connectcheck", CONSUMER_TYPE), "mismatch consumer");
            consumer
                .inputs
                .push(PortDescriptor::iceoryx2("in", "input", schema("AudioFrame")));
            PROCESSOR_REGISTRY
                .register_descriptor_only(consumer)
                .expect("register mismatch consumer descriptor");
        });
    }

    /// Fresh compiler holding one producer node and one consumer node, plus the
    /// wiring refs for their mismatched ports.
    fn compiler_with_mismatched_pair() -> (Arc<Compiler>, OutputLinkPortRef, InputLinkPortRef) {
        ensure_mismatch_types_registered();
        let compiler = Arc::new(Compiler::new());
        let (from_id, to_id): (ProcessorUniqueId, ProcessorUniqueId) =
            compiler.scope(|graph, _tx| {
                let from = graph
                    .traversal_mut()
                    .add_v(ProcessorSpec::new(
                        ident("connectcheck", PRODUCER_TYPE),
                        Value::Null,
                    ))
                    .first()
                    .expect("producer node must be created")
                    .id
                    .clone();
                let to = graph
                    .traversal_mut()
                    .add_v(ProcessorSpec::new(
                        ident("connectcheck", CONSUMER_TYPE),
                        Value::Null,
                    ))
                    .first()
                    .expect("consumer node must be created")
                    .id
                    .clone();
                (from, to)
            });
        (
            compiler,
            OutputLinkPortRef::new(from_id, "out"),
            InputLinkPortRef::new(to_id, "in"),
        )
    }

    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("current-thread runtime")
            .block_on(fut)
    }

    /// Collects the message of every `WARN`-level tracing event so a test can
    /// assert the connect-time schema warn actually fired.
    #[derive(Clone, Default)]
    struct CapturedWarnings(Arc<Mutex<Vec<String>>>);

    struct WarnMessageVisitor<'a>(&'a mut String);
    impl Visit for WarnMessageVisitor<'_> {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                use std::fmt::Write;
                let _ = write!(self.0, "{value:?}");
            }
        }
    }

    impl<S: tracing::Subscriber> Layer<S> for CapturedWarnings {
        fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
            if *event.metadata().level() == tracing::Level::WARN {
                let mut message = String::new();
                event.record(&mut WarnMessageVisitor(&mut message));
                self.0.lock().unwrap().push(message);
            }
        }
    }

    #[test]
    fn loose_connect_warns_but_wires_a_mismatched_link() {
        let (compiler, from, to) = compiler_with_mismatched_pair();
        let warnings = CapturedWarnings::default();
        let subscriber = tracing_subscriber::registry().with(warnings.clone());

        let result = tracing::subscriber::with_default(subscriber, || {
            block_on(connect_impl(
                compiler,
                from,
                to,
                SchemaValidationPosture::Loose,
            ))
        });

        result.expect("loose posture must wire the mismatched link, not fail");

        let captured = warnings.0.lock().unwrap();
        assert!(
            captured
                .iter()
                .any(|m| m.contains("does not match consumer input")),
            "loose connect over a concrete producer/consumer schema mismatch must \
             emit the connect-time warn; captured WARN messages: {captured:?}"
        );
    }

    #[test]
    fn strict_connect_rejects_a_mismatched_link() {
        let (compiler, from, to) = compiler_with_mismatched_pair();
        let err = block_on(connect_impl(
            compiler,
            from,
            to,
            SchemaValidationPosture::Strict,
        ))
        .expect_err("strict posture must reject the mismatched link");
        assert!(
            matches!(err, Error::SchemaIdentMismatch { .. }),
            "strict connect over a concrete schema mismatch must surface \
             Error::SchemaIdentMismatch; got {err:?}"
        );
    }

    /// Criterion-3 revert lock for the *public* strict opt-in: driving the
    /// `Runner::connect_with` authoring surface with [`ConnectOptions::strict`]
    /// over a concrete producer/consumer schema mismatch hard-fails with the
    /// typed [`Error::SchemaIdentMismatch`] and does not wire the link, while
    /// [`ConnectOptions::loose`] over the same pair still wires it.
    ///
    /// Mentally reverting `connect_with` to drop `options.validation` — routing
    /// through the loose `connect` — makes the strict half return `Ok` and fails
    /// here. The `connect_impl`-level tests above never exercise the public
    /// surface, so they don't catch that regression.
    #[test]
    #[serial]
    fn connect_with_strict_rejects_a_mismatched_link_via_the_public_surface() {
        ensure_mismatch_types_registered();
        let runtime = Runner::new().expect("runner builds");

        let producer = runtime
            .add_processor(ProcessorSpec::new(
                ident("connectcheck", PRODUCER_TYPE),
                Value::Null,
            ))
            .expect("producer node adds");
        let consumer = runtime
            .add_processor(ProcessorSpec::new(
                ident("connectcheck", CONSUMER_TYPE),
                Value::Null,
            ))
            .expect("consumer node adds");

        let err = runtime
            .connect_with(
                OutputLinkPortRef::new(producer.clone(), "out"),
                InputLinkPortRef::new(consumer.clone(), "in"),
                ConnectOptions::strict(),
            )
            .expect_err("strict connect_with must reject the mismatched link");
        assert!(
            matches!(err, Error::SchemaIdentMismatch { .. }),
            "public strict opt-in must surface Error::SchemaIdentMismatch; got {err:?}"
        );

        runtime
            .connect_with(
                OutputLinkPortRef::new(producer, "out"),
                InputLinkPortRef::new(consumer, "in"),
                ConnectOptions::loose(),
            )
            .expect("loose connect_with over the same pair must still wire the link");
    }

    /// Async counterpart to
    /// [`connect_with_strict_rejects_a_mismatched_link_via_the_public_surface`]:
    /// awaiting `Runner::connect_with_async` from inside a tokio task under
    /// [`ConnectOptions::strict`] hard-fails a concrete producer/consumer schema
    /// mismatch with the typed [`Error::SchemaIdentMismatch`] and does not wire
    /// the link, while [`ConnectOptions::loose`] over the same pair
    /// still wires it.
    ///
    /// Mentally reverting `connect_with_async` to thread a hardcoded `Loose`
    /// posture instead of `options.validation` makes the strict half return `Ok`
    /// and fails here — the sync `connect_with` test never drives the async
    /// opt-in path, so it doesn't catch that regression.
    #[test]
    #[serial]
    fn connect_with_async_strict_rejects_a_mismatched_link_via_the_public_surface() {
        ensure_mismatch_types_registered();
        let runtime = Runner::new().expect("runner builds");

        let producer = runtime
            .add_processor(ProcessorSpec::new(
                ident("connectcheck", PRODUCER_TYPE),
                Value::Null,
            ))
            .expect("producer node adds");
        let consumer = runtime
            .add_processor(ProcessorSpec::new(
                ident("connectcheck", CONSUMER_TYPE),
                Value::Null,
            ))
            .expect("consumer node adds");

        let err = block_on(runtime.connect_with_async(
            OutputLinkPortRef::new(producer.clone(), "out"),
            InputLinkPortRef::new(consumer.clone(), "in"),
            ConnectOptions::strict(),
        ))
        .expect_err("strict connect_with_async must reject the mismatched link");
        assert!(
            matches!(err, Error::SchemaIdentMismatch { .. }),
            "public async strict opt-in must surface Error::SchemaIdentMismatch; got {err:?}"
        );

        block_on(runtime.connect_with_async(
            OutputLinkPortRef::new(producer, "out"),
            InputLinkPortRef::new(consumer, "in"),
            ConnectOptions::loose(),
        ))
        .expect("loose connect_with_async over the same pair must still wire the link");
    }
}

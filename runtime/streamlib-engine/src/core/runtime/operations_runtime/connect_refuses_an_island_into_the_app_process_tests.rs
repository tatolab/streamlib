// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `connect` refuses a link from a function node into a node that runs in the app process, on a
//! stream no commit builds, in the words its wiring would refuse it in at play, so the link is
//! never in the graph a stopped stream saves.

use std::sync::Arc;

use serde_json::Value;

use super::{LiveGraphChangeOnOneStream, connect_impl};
use crate::core::compiler::Compiler;
use crate::core::descriptors::{
    FunctionNodeLanguage, NodeClassImportPath, NodeClassShortName, NodeDescriptor, NodeKind,
    PortDescriptor, PortFamily, PortFamilyDeclaration,
};
use crate::core::graph::input_values::LiveInputValuesOfAStream;
use crate::core::graph::{InputLinkPortRef, LinkUniqueId, NodeRunId, OutputLinkPortRef};
use crate::core::nodes::{HowANodeOfADescribedTypeRuns, NodeSpec, NodeTypesOneStreamResolves};
use crate::core::pubsub::LoadedStreamIdentity;
use crate::core::test_support::{MockInputOnlyNode, ensure_test_mocks_registered};
use crate::core::{Error, Result};

const FUNCTION_NODE: &str = "app_process_check:switch";
const HELPER_PROCESS_READER: &str = "app_process_check:meter";

fn a_records_port(name: &str) -> PortDescriptor {
    PortDescriptor::new(
        name,
        "",
        true,
        PortFamilyDeclaration::of_family(PortFamily::Records),
    )
    .with_delivery_profile("newest")
}

fn a_described_node_type(
    import_path: &str,
    node_kind: NodeKind,
    inputs: &[&str],
    outputs: &[&str],
) -> NodeDescriptor {
    let import_path = NodeClassImportPath::new(import_path).unwrap();
    let short_name = import_path.as_str().rsplit(':').next().unwrap().to_owned();
    let mut descriptor = NodeDescriptor::new(
        NodeClassShortName::new(short_name).unwrap(),
        import_path,
        "a fixture",
    );
    descriptor.node_kind = node_kind;
    descriptor.inputs = inputs.iter().map(|name| a_records_port(name)).collect();
    descriptor.outputs = outputs.iter().map(|name| a_records_port(name)).collect();
    descriptor
}

/// A compiler of a stopped stream resolving a function node type, `switch`, and a type its node
/// code runs in a helper process, `meter`, beside the natively compiled mocks.
fn a_stopped_stream_resolving_a_function_node_type() -> Arc<Compiler> {
    ensure_test_mocks_registered();
    let node_types = NodeTypesOneStreamResolves::new();
    node_types
        .register_a_type_described_in_this_streams_node_interpreter(
            a_described_node_type(
                FUNCTION_NODE,
                NodeKind::Function {
                    language: FunctionNodeLanguage::Python,
                },
                &["x"],
                &["result"],
            ),
            HowANodeOfADescribedTypeRuns::InsideTheReadOfEachNodeItFeeds {
                node_once_promoted_to_its_own_process: Box::new(|_node| {
                    Err(Error::NotSupported("a fixture runs no node".to_owned()))
                }),
            },
        )
        .unwrap();
    node_types
        .register_a_type_described_in_this_streams_node_interpreter(
            a_described_node_type(HELPER_PROCESS_READER, NodeKind::Process, &["in1"], &[]),
            HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(Box::new(|_node| {
                Err(Error::NotSupported("a fixture runs no node".to_owned()))
            })),
        )
        .unwrap();
    Arc::new(Compiler::new_of_a_stream(
        Arc::new(node_types),
        LiveInputValuesOfAStream::default(),
    ))
}

fn a_node_of(compiler: &Compiler, node_type: &str) -> NodeRunId {
    compiler.scope(|graph, _tx| {
        graph
            .traversal_mut()
            .add_v(NodeSpec::new(
                NodeClassImportPath::new(node_type).unwrap(),
                Value::Null,
            ))
            .expect("the node is named")
            .first()
            .expect("the node is created")
            .id
            .clone()
    })
}

fn connect_on_this_thread(
    compiler: &Arc<Compiler>,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
) -> Result<LinkUniqueId> {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("current-thread runtime")
        .block_on(connect_impl(
            LiveGraphChangeOnOneStream {
                compiler: Arc::clone(compiler),
                live: None,
                stream: LoadedStreamIdentity {
                    runtime_id: "Rappprocesscheck".to_string(),
                    stream_id: "main".to_string(),
                    stream_tag: crate::core::runtime::LoadedStreamTag::numbered_for_a_test(1),
                },
            },
            from,
            to,
        ))
}

fn the_links_in(compiler: &Compiler) -> usize {
    compiler.scope(|graph, _tx| graph.traversal().e(()).iter().count())
}

#[test]
fn a_function_node_into_a_natively_compiled_node_is_refused_and_never_made() {
    let compiler = a_stopped_stream_resolving_a_function_node_type();
    let switch = a_node_of(&compiler, FUNCTION_NODE);
    let program = a_node_of(
        &compiler,
        MockInputOnlyNode::node_class_import_path().as_str(),
    );

    let refusal = connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(switch.clone(), "result"),
        InputLinkPortRef::new(program.clone(), "in1"),
    )
    .expect_err("the app process runs no function node");

    assert!(matches!(refusal, Error::InvalidLink(_)), "{refusal:?}");
    assert_eq!(
        refusal.to_string(),
        Error::InvalidLink(format!(
            "the link from `{switch}.result` to `{program}.in1` feeds a function node island \
             into `{program}`, which runs in the app process; a function node runs inside the \
             read of a node in a helper process, and the app process runs no node code"
        ))
        .to_string()
    );
    assert_eq!(the_links_in(&compiler), 0);
}

#[test]
fn a_function_node_into_a_node_its_node_code_runs_in_a_helper_process_connects() {
    let compiler = a_stopped_stream_resolving_a_function_node_type();
    let switch = a_node_of(&compiler, FUNCTION_NODE);
    let meter = a_node_of(&compiler, HELPER_PROCESS_READER);

    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(switch, "result"),
        InputLinkPortRef::new(meter, "in1"),
    )
    .expect("a helper process evaluates the island at each read");
    assert_eq!(the_links_in(&compiler), 1);
}

#[test]
fn a_function_node_into_another_function_node_connects() {
    let compiler = a_stopped_stream_resolving_a_function_node_type();
    let upstream = a_node_of(&compiler, FUNCTION_NODE);
    let downstream = a_node_of(&compiler, FUNCTION_NODE);

    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(upstream, "result"),
        InputLinkPortRef::new(downstream, "x"),
    )
    .expect("the two run in one island");
    assert_eq!(the_links_in(&compiler), 1);
}

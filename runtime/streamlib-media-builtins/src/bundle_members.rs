// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Built-ins `MakeBundle` and `BreakBundle`: a bundle made from a wire into each of its members,
//! and a bundle broken into a wire out of each. Both carry the bundle their `bundle_type` input
//! chooses on one input and one output. A link may end at a member of `MakeBundle`'s input, which
//! takes the newest value of each, and a member no link carries reads the value set on it; a link
//! may start at a member of `BreakBundle`'s output, which carries that member alone. Each message
//! passes through as it arrives, so a bundle holding a Texture or Audio member passes by reference,
//! as a node's own bundle pin carries one.

use streamlib::sdk::context::RuntimeContextLimitedAccess;
use streamlib::sdk::descriptors::PinType;
use streamlib::sdk::error::Result;
use streamlib::sdk::iceoryx2::{InputMailboxes, OutputWriter};
use streamlib::sdk::nodes::Input;

const MEMBERS_INPUT_PORT: &str = "members";
const BUNDLE_OUTPUT_PORT: &str = "bundle";
const BUNDLE_INPUT_PORT: &str = "bundle";
const MEMBERS_OUTPUT_PORT: &str = "members";

#[streamlib::sdk::node(
    built_in_node,
    execution = reactive,
    input(
        "bundle_type",
        description = "The bundle the node carries: one a node type of the stream declares.",
        value = Option<PinType>,
        default = None,
        among_the_bundles_its_stream_declares = true
    ),
    input(
        "members",
        description = "The bundle's members, each wired or set on its own",
        delivery_profile = "ordered",
        pin_type = from_input(bundle_type),
        takes_a_value = true
    ),
    output(
        "bundle",
        description = "One bundle built from the newest value of each member",
        pin_type = from_input(bundle_type)
    ),
)]
#[meta(
    description = "Makes one bundle of `bundle_type` from its members. A wire ends at each \
                   member of `members`, and a member no wire reaches reads the value set on it. \
                   Each message a member wire carries sends one bundle on `bundle`, built from \
                   the newest value of every member and stamped as that message; none is sent \
                   while a member that declares no default holds no value. A bundle wired whole \
                   into `members` passes through as it is, a Texture or Audio member by \
                   reference."
)]
pub fn make_bundle(
    bundle_type: Input<Option<PinType>>,
    inputs: InputMailboxes,
    outputs: OutputWriter,
) -> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()> {
    // `bundle_type` types both ports, and no frame reads it.
    let _ = bundle_type;
    passing_each_message_through(inputs, MEMBERS_INPUT_PORT, outputs, BUNDLE_OUTPUT_PORT)
}

#[streamlib::sdk::node(
    built_in_node,
    execution = reactive,
    input(
        "bundle_type",
        description = "The bundle the node carries: one a node type of the stream declares.",
        value = Option<PinType>,
        default = None,
        among_the_bundles_its_stream_declares = true
    ),
    input(
        "bundle",
        description = "The bundle to break into its members",
        delivery_profile = "ordered",
        pin_type = from_input(bundle_type)
    ),
    output(
        "members",
        description = "The bundle's members, a wire starting at each",
        pin_type = from_input(bundle_type)
    ),
)]
#[meta(
    description = "Breaks each bundle of `bundle_type` on `bundle` into its members. A wire \
                   starts at each member of `members` and carries that member alone, typed as \
                   it, so several nodes each read the member they take. A Texture or Audio \
                   member is carried by reference, as a node's own bundle pin carries one."
)]
pub fn break_bundle(
    bundle_type: Input<Option<PinType>>,
    inputs: InputMailboxes,
    outputs: OutputWriter,
) -> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()> {
    // `bundle_type` types both ports, and no frame reads it.
    let _ = bundle_type;
    passing_each_message_through(inputs, BUNDLE_INPUT_PORT, outputs, MEMBERS_OUTPUT_PORT)
}

/// The per-frame function writing each message waiting on `input_port` of `inputs` to
/// `output_port` of `outputs` as it arrived: its bytes, its stamp, its group boundary and the
/// descriptor of its payload.
fn passing_each_message_through(
    inputs: InputMailboxes,
    input_port: &'static str,
    outputs: OutputWriter,
    output_port: &'static str,
) -> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()> + Send + 'static {
    move |_ctx| {
        while let Some(message) = inputs.read_raw(input_port)? {
            outputs.write_bytes_described_by(
                output_port,
                message.payload.bytes(),
                message.timestamp_ns,
                message.group_boundary,
                message.payload_descriptor,
            )?;
        }
        Ok(())
    }
}

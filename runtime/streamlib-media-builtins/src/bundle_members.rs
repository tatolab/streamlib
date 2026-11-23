// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Built-ins `MakeBundle` and `BreakBundle`: a bundle made from a wire into each of its members,
//! and a bundle broken into a wire out of each. Both carry the bundle their `bundle_type` setting
//! chooses on one input and one output. A link may end at a member of `MakeBundle`'s input, which
//! takes the newest value of each, and a member no link carries reads the value set on it; a link
//! may start at a member of `BreakBundle`'s output, which carries that member alone. Each message
//! passes through as it arrives, so a bundle holding a Texture or Audio member passes by reference,
//! as a node's own bundle pin carries one.

use serde::{Deserialize, Serialize};
use streamlib::sdk::context::RuntimeContextLimitedAccess;
use streamlib::sdk::descriptors::PinType;
use streamlib::sdk::error::Result;
use streamlib::sdk::iceoryx2::{InputMailboxes, OutputWriter};
use streamlib::sdk::nodes::ReactiveNode;

const MEMBERS_INPUT_PORT: &str = "members";
const BUNDLE_OUTPUT_PORT: &str = "bundle";
const BUNDLE_INPUT_PORT: &str = "bundle";
const MEMBERS_OUTPUT_PORT: &str = "members";

/// The setting [`MakeBundle`] and [`BreakBundle`] read at setup: the bundle they carry.
#[streamlib::sdk::node_settings]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleTypeSettings {
    #[description = "The bundle the node carries: one a module of the stream declares."]
    pub bundle_type: Option<PinType>,
}

#[streamlib::sdk::node(
    built_in_node,
    execution = reactive,
    settings = crate::bundle_members::BundleTypeSettings,
    input(
        "members",
        description = "The bundle's members, each wired or set on its own",
        delivery_profile = "ordered",
        family = Records,
        pin_type = from_setting("bundle_type"),
        takes_a_value = true
    ),
    output(
        "bundle",
        description = "One bundle built from the newest value of each member",
        family = Records,
        pin_type = from_setting("bundle_type")
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
pub struct MakeBundle;

impl ReactiveNode for MakeBundle::Node {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        pass_each_message_through(
            &self.inputs,
            MEMBERS_INPUT_PORT,
            &self.outputs,
            BUNDLE_OUTPUT_PORT,
        )
    }
}

#[streamlib::sdk::node(
    built_in_node,
    execution = reactive,
    settings = crate::bundle_members::BundleTypeSettings,
    input(
        "bundle",
        description = "The bundle to break into its members",
        delivery_profile = "ordered",
        family = Records,
        pin_type = from_setting("bundle_type")
    ),
    output(
        "members",
        description = "The bundle's members, a wire starting at each",
        family = Records,
        pin_type = from_setting("bundle_type")
    ),
)]
#[meta(
    description = "Breaks each bundle of `bundle_type` on `bundle` into its members. A wire \
                   starts at each member of `members` and carries that member alone, typed as \
                   it, so several nodes each read the member they take. A Texture or Audio \
                   member is carried by reference, as a node's own bundle pin carries one."
)]
pub struct BreakBundle;

impl ReactiveNode for BreakBundle::Node {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        pass_each_message_through(
            &self.inputs,
            BUNDLE_INPUT_PORT,
            &self.outputs,
            MEMBERS_OUTPUT_PORT,
        )
    }
}

/// Write each message waiting on `input_port` of `inputs` to `output_port` of `outputs` as it
/// arrived: its bytes, its stamp, its group boundary and the descriptor of its payload.
fn pass_each_message_through(
    inputs: &InputMailboxes,
    input_port: &str,
    outputs: &OutputWriter,
    output_port: &str,
) -> Result<()> {
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

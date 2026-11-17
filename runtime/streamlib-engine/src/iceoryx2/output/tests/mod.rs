// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::channel_egress::{
    a_send_may_have_delivered, sequence_number_following_a_send, trust_tier_label,
};
use super::*;
use crate::core::error::ChannelTrustTierLabel;
use crate::core::machine_global_unique_name::mint_machine_global_unique_name_suffix;
use crate::iceoryx2::channel_sample_test_support::stamped_payload_of;
use crate::iceoryx2::{
    ChannelTrustTier, Iceoryx2Node, Iceoryx2Service, data_channel_payload_cache_lines_for,
};
use iceoryx2::port::SendError;

/// Each test gets a unique service-name prefix so parallel invocations
/// don't collide on iceoryx2's machine-global `/dev/shm` namespace.
fn unique_suffix(tag: &str) -> String {
    format!(
        "test/output/{tag}/{}",
        mint_machine_global_unique_name_suffix()
    )
}

/// The refused-bag count `graph` would render for `output_port`.
fn refused_bag_count_of(inner: &OutputWriterInner, output_port: &str) -> u64 {
    inner
        .bag_counts_by_output_port()
        .refused_bag_count_snapshot_by_output_port()[output_port]
}

fn written_bag_count_of(inner: &OutputWriterInner, output_port: &str) -> u64 {
    inner
        .bag_counts_by_output_port()
        .written_bag_count_snapshot_by_output_port()[output_port]
}

/// A writer whose one output port `out` publishes onto `pubsub` under a
/// `chunk_ceiling_bytes` ceiling.
fn output_writer_with_one_channel(
    pubsub: &Iceoryx2Service,
    chunk_ceiling_bytes: usize,
) -> OutputWriterInner {
    let inner = OutputWriterInner::new();
    inner.set_channel_publisher(
        "out",
        pubsub,
        pubsub.create_publisher(64).unwrap(),
        ChannelEgressConfig {
            service_name: "test/out".to_string(),
            trust_tier: ChannelTrustTier::Trusted,
            expected_payload_bytes: 64,
            chunk_ceiling_bytes,
        },
    );
    inner
}

/// A channel data service opened the way the engine opens one, sized for
/// `max_subscribers` rings of four.
fn open_channel_data_service(tag: &str, max_subscribers: usize) -> Iceoryx2Service {
    Iceoryx2Node::for_this_test_process()
        .open_or_create_service(&unique_suffix(tag), max_subscribers, 4)
        .expect("open the channel data service")
}

mod output_port_kept_last_message_tests;
mod output_port_link_and_loss_accounting_tests;
mod output_port_write_and_loan_tests;

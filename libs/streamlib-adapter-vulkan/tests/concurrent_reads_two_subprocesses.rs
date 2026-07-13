// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Two subprocesses simultaneously hold read access on the same surface
//! (each via its own imported VkImage + timeline). Exercises the
//! emergent multi-process-concurrent-consumer pattern of the pre-lift
//! single-timeline adapter shape.
//!
//! **Deferred under the single-writer-per-edge lift** per
//! `docs/architecture/adapter-timeline-single-writer.md` — the v1
//! model declares one producer process + one consumer process per
//! surface, fixed at registration time. Two subprocesses concurrently
//! holding read access on the same surface is the
//! multi-process-concurrent-consumer pattern explicitly deferred for
//! v1. The additive extension when it's needed is N `consume_done`
//! timelines (one per attached consumer process), not a single
//! shared timeline.
//!
//! Same-process concurrent reads (multiple threads inside one
//! subprocess, or multiple in-process consumers in the host) remain
//! fully supported via the existing `read_holders` last-reader-out
//! semantics — see `conformance.rs` and `write_excludes_read.rs` for
//! coverage that exercises them.

#![cfg(target_os = "linux")]

#[path = "common.rs"]
mod common;

use std::sync::Arc;
use streamlib::sdk::engine::HostTextureExt;

/// Two subprocesses concurrently reading the same surface — out of
/// scope for v1 per the single-writer-per-edge model. Left in-tree as
/// a `#[ignore]`d marker so the deferred capability is visible when
/// re-reading the test suite; do not lift the `#[ignore]` without
/// landing the N-`consume_done`-timelines extension first.
#[test]
#[ignore = "v1 single-writer-per-edge defers multi-process concurrent consumers — \
            see docs/architecture/adapter-timeline-single-writer.md"]
fn two_subprocesses_concurrently_read_same_surface() {
    let host = match common::HostFixture::try_new() {
        Some(h) => h,
        None => {
            tracing::info!("concurrent_reads_two_subprocesses: skipping — no Vulkan");
            return;
        }
    };

    let surface = host.register_surface(33, 64, 64);

    // Warm up the surface — host writes once so subprocesses have a
    // known starting layout.
    {
        let _w = host
            .ctx
            .acquire_write(&surface.descriptor)
            .expect("warm-up write");
    }
    assert_eq!(surface.produce_done.current_value().unwrap(), 1);

    // Spawn two subprocesses, each waits on produce_done and signals
    // consume_done. Under single-writer-per-edge both subprocesses
    // writing to the same consume_done timeline races VUID-03258 — this
    // is exactly the pattern the v1 model defers.
    let (mut child_a, sock_a) = common::spawn_helper("wait-only");
    let (mut child_b, sock_b) = common::spawn_helper("wait-only");

    // Each subprocess imports its own copy of both timelines + the
    // DMA-BUF — fresh fds per spawn since SCM_RIGHTS dups by the kernel.
    for sock in [&sock_a, &sock_b] {
        let dma_buf_fd = surface
            .texture
            .vulkan_inner()
            .export_dma_buf_fd()
            .expect("export DMA-BUF");
        let produce_done_fd = Arc::clone(&surface.produce_done)
            .export_opaque_fd()
            .expect("export produce_done_fd");
        let consume_done_fd = Arc::clone(&surface.consume_done)
            .export_opaque_fd()
            .expect("export consume_done_fd");
        let req = common::helper_descriptor("wait-only", &surface, 1, None);
        common::send_helper_request(sock, &req, &[dma_buf_fd], produce_done_fd, consume_done_fd)
            .expect("send helper request");
    }

    let resp_a = common::recv_helper_response(&sock_a);
    let resp_b = common::recv_helper_response(&sock_b);
    assert_eq!(resp_a["ok"], true, "child A: {}", resp_a["note"]);
    assert_eq!(resp_b["ok"], true, "child B: {}", resp_b["note"]);

    assert_eq!(child_a.wait().expect("wait a").code(), Some(0));
    assert_eq!(child_b.wait().expect("wait b").code(), Some(0));

    // Same-process concurrent reads remain supported; exercise them
    // here for parity with the deferred cross-process pattern.
    let r1 = host.ctx.acquire_read(&surface.descriptor).expect("read 1");
    let r2 = host.ctx.acquire_read(&surface.descriptor).expect("read 2");
    drop(r1);
    drop(r2);
}

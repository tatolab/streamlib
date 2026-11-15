// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::rhi::RhiExternalHandle;

/// An OPAQUE_FD-registered surface must import as OPAQUE_FD. This path is
/// reachable the moment a helper process checks out device-export staging,
/// and the failure it prevents is a wrong-type import inside the driver
/// rather than an error the caller could see.
#[test]
fn an_opaque_fd_surface_imports_as_opaque_fd_at_the_stated_memory_type_index() {
    let handles = external_plane_handles_for_flavour(
        "check_out",
        SURFACE_HANDLE_TYPE_OPAQUE_FD,
        &[7, 8],
        &[64, 128],
        Some(0),
    )
    .unwrap();
    assert!(matches!(
        handles.as_slice(),
        [
            RhiExternalHandle::OpaqueFd {
                fd: 7,
                size: 64,
                exporting_allocation_memory_type_index: 0
            },
            RhiExternalHandle::OpaqueFd {
                fd: 8,
                size: 128,
                exporting_allocation_memory_type_index: 0
            },
        ]
    ));
}

/// OPAQUE_FD has no fd-properties query, so a surface that never stated
/// the exporter's memory type index has no conforming import; it is
/// refused by name rather than imported at a guessed type.
#[test]
fn an_opaque_fd_surface_without_a_stated_memory_type_index_is_refused_by_name() {
    let refusal = external_plane_handles_for_flavour(
        "lookup",
        SURFACE_HANDLE_TYPE_OPAQUE_FD,
        &[7],
        &[64],
        None,
    )
    .unwrap_err();
    let refusal = refusal.to_string();
    assert!(refusal.contains("lookup"), "{refusal}");
    assert!(refusal.contains("vk_memory_type_index"), "{refusal}");
}

#[test]
fn a_dma_buf_surface_imports_as_dma_buf() {
    let handles = external_plane_handles_for_flavour(
        "check_out",
        SURFACE_HANDLE_TYPE_DMA_BUF,
        &[3],
        &[4096],
        None,
    )
    .unwrap();
    assert!(matches!(
        handles.as_slice(),
        [RhiExternalHandle::DmaBuf { fd: 3, size: 4096 }]
    ));
}

/// A surface registered before the flavour was expressible sends no
/// `handle_type`; the caller substitutes the DMA-BUF default, so every
/// such surface keeps importing exactly as it did.
#[test]
fn the_absent_flavour_default_is_dma_buf() {
    let handles = external_plane_handles_for_flavour(
        "check_out",
        SURFACE_HANDLE_TYPE_DMA_BUF,
        &[3],
        &[0],
        None,
    )
    .unwrap();
    assert!(matches!(
        handles.as_slice(),
        [RhiExternalHandle::DmaBuf { fd: 3, .. }]
    ));
}

#[test]
fn an_unknown_flavour_is_refused_by_name() {
    let refusal = external_plane_handles_for_flavour("check_out", "io_surface", &[3], &[0], None)
        .unwrap_err();
    assert!(
        refusal.to_string().contains("io_surface"),
        "the refusal must name what it got: {refusal}"
    );
}

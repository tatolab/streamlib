// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::rhi::RhiExternalHandle;
use std::os::fd::{AsRawFd as _, FromRawFd as _};

/// A real fd standing in for one an exporter minted, paired with the
/// identity that outlives its number being recycled — so "was this
/// closed?" can be answered without racing a parallel test thread for
/// the descriptor. A pipe rather than a device file because each pipe
/// gets its own inode, and holding the write end keeps that inode
/// alive even after the read end closes.
struct ExportedPlaneFdUnderTest {
    plane_fd: std::os::unix::io::RawFd,
    write_end_fd: std::os::unix::io::RawFd,
    inode: u64,
}

fn inode_of(fd: std::os::unix::io::RawFd) -> Option<u64> {
    let mut file_status = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(fd, file_status.as_mut_ptr()) } != 0 {
        return None;
    }
    Some(unsafe { file_status.assume_init() }.st_ino)
}

impl ExportedPlaneFdUnderTest {
    fn mint() -> Self {
        let mut pipe_ends = [0 as std::os::unix::io::RawFd; 2];
        assert_eq!(
            unsafe { libc::pipe2(pipe_ends.as_mut_ptr(), libc::O_CLOEXEC) },
            0,
            "could not mint a stand-in plane fd"
        );
        let inode = inode_of(pipe_ends[0]).expect("a fresh pipe must stat");
        Self {
            plane_fd: pipe_ends[0],
            write_end_fd: pipe_ends[1],
            inode,
        }
    }

    /// Closed outright, or its number recycled onto something else —
    /// either way the descriptor is no longer the one we handed over.
    fn was_closed(&self) -> bool {
        inode_of(self.plane_fd) != Some(self.inode)
    }
}

impl Drop for ExportedPlaneFdUnderTest {
    fn drop(&mut self) {
        unsafe {
            if !self.was_closed() {
                libc::close(self.plane_fd);
            }
            libc::close(self.write_end_fd);
        }
    }
}

/// A single-flavour export holds every plane fd open for the send, and
/// dropping the wire handles is the one close each fd gets.
#[test]
fn a_single_flavour_export_owns_every_plane_fd_until_the_send_is_done() {
    let first_plane = ExportedPlaneFdUnderTest::mint();
    let second_plane = ExportedPlaneFdUnderTest::mint();

    let wire_handles = adopt_exported_planes_into_wire_handles(
        vec![
            RhiExternalHandle::DmaBuf {
                fd: first_plane.plane_fd,
                size: 4096,
            },
            RhiExternalHandle::DmaBuf {
                fd: second_plane.plane_fd,
                size: 2048,
            },
        ],
        [0; 16],
    )
    .expect("a single-flavour export must be accepted");

    assert_eq!(
        wire_handles.plane_flavour,
        ExportedPlaneFlavourOnTheWire::DmaBuf
    );
    assert_eq!(wire_handles.plane_sizes, vec![4096, 2048]);
    assert_eq!(wire_handles.plane_offsets, vec![0, 0]);
    assert!(
        !first_plane.was_closed() && !second_plane.was_closed(),
        "the plane fds must still be open — the send has not happened yet"
    );

    drop(wire_handles);

    assert!(
        first_plane.was_closed() && second_plane.was_closed(),
        "dropping the wire handles must close every plane fd it adopted"
    );
}

/// An OPAQUE_FD export carries its contract onto the wire: the memory
/// type index the handle stated and the exporting device's UUID in hex. A
/// DMA-BUF export carries none.
#[test]
fn an_opaque_fd_export_states_the_handles_memory_type_index_and_the_exporting_device() {
    let opaque_fd_plane = ExportedPlaneFdUnderTest::mint();
    let exporting_device_uuid: [u8; 16] = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ];
    let wire_handles = adopt_exported_planes_into_wire_handles(
        vec![RhiExternalHandle::OpaqueFd {
            fd: opaque_fd_plane.plane_fd,
            size: 4096,
            exporting_allocation_memory_type_index: 0,
        }],
        exporting_device_uuid,
    )
    .expect("a single OPAQUE_FD plane is accepted");
    assert_eq!(
        wire_handles.plane_flavour,
        ExportedPlaneFlavourOnTheWire::OpaqueFd(OpaqueFdExportContract {
            vk_memory_type_index: 0,
            exporting_device_uuid: "00112233445566778899aabbccddeeff".to_string(),
        })
    );
    drop(wire_handles);
    assert!(opaque_fd_plane.was_closed());

    let dma_buf_plane = ExportedPlaneFdUnderTest::mint();
    let wire_handles = adopt_exported_planes_into_wire_handles(
        vec![RhiExternalHandle::DmaBuf {
            fd: dma_buf_plane.plane_fd,
            size: 4096,
        }],
        exporting_device_uuid,
    )
    .expect("a DMA-BUF plane is accepted");
    assert_eq!(
        wire_handles.plane_flavour,
        ExportedPlaneFlavourOnTheWire::DmaBuf
    );
}

/// The refusal path obeys the same rule as the send path. A plane fd
/// left open here is one no owner ever closes; a plane fd closed twice
/// corrupts whichever subsystem the kernel hands the number to next,
/// which is the teardown crash in #1880.
#[test]
fn a_mixed_flavour_export_is_refused_and_closes_every_plane_it_adopted() {
    let first_plane = ExportedPlaneFdUnderTest::mint();
    let second_plane = ExportedPlaneFdUnderTest::mint();

    let refusal = match adopt_exported_planes_into_wire_handles(
        vec![
            RhiExternalHandle::DmaBuf {
                fd: first_plane.plane_fd,
                size: 4096,
            },
            RhiExternalHandle::OpaqueFd {
                fd: second_plane.plane_fd,
                size: 4096,
                exporting_allocation_memory_type_index: 0,
            },
        ],
        [0; 16],
    ) {
        Ok(_) => panic!("a buffer exporting two flavours must be refused"),
        Err(refusal) => refusal,
    };

    assert!(
        refusal
            .to_string()
            .contains("mixed external-handle flavours"),
        "the refusal must name what it got: {refusal}"
    );
    assert!(
        first_plane.was_closed(),
        "the refusal left plane 0 open — no owner remains to close it"
    );
    assert!(
        second_plane.was_closed(),
        "the refusal left plane 1 open — no owner remains to close it"
    );
}

/// One registration states one memory type index, so OPAQUE_FD planes
/// exported at two are refused by name, and every plane adopted closes.
#[test]
fn opaque_fd_planes_at_different_memory_type_indices_are_refused_and_close_every_plane() {
    let first_plane = ExportedPlaneFdUnderTest::mint();
    let second_plane = ExportedPlaneFdUnderTest::mint();

    let refusal = match adopt_exported_planes_into_wire_handles(
        vec![
            RhiExternalHandle::OpaqueFd {
                fd: first_plane.plane_fd,
                size: 4096,
                exporting_allocation_memory_type_index: 0,
            },
            RhiExternalHandle::OpaqueFd {
                fd: second_plane.plane_fd,
                size: 4096,
                exporting_allocation_memory_type_index: 3,
            },
        ],
        [0; 16],
    ) {
        Ok(_) => panic!("OPAQUE_FD planes at two memory type indices must be refused"),
        Err(refusal) => refusal,
    };

    assert!(
        refusal
            .to_string()
            .contains("OPAQUE_FD planes at different memory type indices ([0, 3])"),
        "the refusal must name what it got: {refusal}"
    );
    assert!(
        first_plane.was_closed() && second_plane.was_closed(),
        "the refusal must close every plane fd it adopted"
    );
}

fn a_reply_announcing(produce_done: bool, consume_done: bool) -> serde_json::Value {
    serde_json::json!({
        SURFACE_REPLY_HAS_PRODUCE_DONE_FD: produce_done,
        SURFACE_REPLY_HAS_CONSUME_DONE_FD: consume_done,
    })
}

/// Stand-in fds handed over as the reply's owned fds, in order. The
/// stand-ins outlive the owned fds so they can say which ones closed.
fn reply_fds_standing_in_for(stand_ins: &[ExportedPlaneFdUnderTest]) -> Vec<OwnedFd> {
    stand_ins
        .iter()
        .map(|stand_in| unsafe { OwnedFd::from_raw_fd(stand_in.plane_fd) })
        .collect()
}

fn raw_fds_of(owned: &[OwnedFd]) -> Vec<std::os::unix::io::RawFd> {
    owned.iter().map(OwnedFd::as_raw_fd).collect()
}

#[test]
fn a_reply_announcing_no_edges_keeps_every_fd_as_a_plane() {
    let stand_ins: Vec<_> = (0..3).map(|_| ExportedPlaneFdUnderTest::mint()).collect();
    let planes = plane_fds_of_reply(
        "lookup",
        &a_reply_announcing(false, false),
        reply_fds_standing_in_for(&stand_ins),
    )
    .expect("three fds and no edges are three planes");
    assert_eq!(
        raw_fds_of(&planes),
        stand_ins.iter().map(|p| p.plane_fd).collect::<Vec<_>>()
    );
    assert!(stand_ins.iter().all(|plane| !plane.was_closed()));
    drop(planes);
    assert!(stand_ins.iter().all(|plane| plane.was_closed()));
}

/// The service appends `produce_done` then `consume_done` after the
/// planes; each announced edge comes off the end and closes, and the
/// planes in front of them are untouched.
#[test]
fn each_announced_edge_is_peeled_off_the_end_and_closed() {
    for (produce_done, consume_done) in [(true, false), (false, true), (true, true)] {
        let edge_count = usize::from(produce_done) + usize::from(consume_done);
        let stand_ins: Vec<_> = (0..2 + edge_count)
            .map(|_| ExportedPlaneFdUnderTest::mint())
            .collect();
        let planes = plane_fds_of_reply(
            "lookup",
            &a_reply_announcing(produce_done, consume_done),
            reply_fds_standing_in_for(&stand_ins),
        )
        .expect("two planes remain once the edges are peeled");
        assert_eq!(
            raw_fds_of(&planes),
            stand_ins[..2]
                .iter()
                .map(|p| p.plane_fd)
                .collect::<Vec<_>>(),
            "produce_done={produce_done} consume_done={consume_done}"
        );
        assert!(
            stand_ins[2..].iter().all(|edge| edge.was_closed()),
            "an announced edge must close with the peel (produce_done={produce_done} consume_done={consume_done})"
        );
        assert!(stand_ins[..2].iter().all(|plane| !plane.was_closed()));
        drop(planes);
    }
}

#[test]
fn a_reply_shorter_than_its_flags_promise_is_refused_and_closes_what_it_carried() {
    let stand_ins: Vec<_> = (0..2).map(|_| ExportedPlaneFdUnderTest::mint()).collect();
    let refusal = plane_fds_of_reply(
        "lookup",
        &a_reply_announcing(true, true),
        reply_fds_standing_in_for(&stand_ins),
    )
    .expect_err("two fds cannot carry a plane and two edges");
    assert!(
        refusal
            .to_string()
            .contains("carried 2 fds, fewer than the 3"),
        "the refusal must name both counts: {refusal}"
    );
    assert!(
        stand_ins.iter().all(|fd| fd.was_closed()),
        "a refused reply must close every fd it carried"
    );
}

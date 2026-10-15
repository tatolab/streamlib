// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::net::UnixStream;

use pyo3::types::PyModule;
use streamlib::sdk::engine::host_rhi::{
    HostVulkanBuffer, HostVulkanDevice, HostVulkanTimelineSemaphore,
};

use super::*;
use crate::python_surface_share_service_for_tests::SurfaceShareUnderTest;

const STAGING_WIDTH: u32 = 64;
const STAGING_HEIGHT: u32 = 16;
const STAGING_BYTE_SIZE: u64 = 64 * 4 * 16;
const ENGINE_COPIED_STAMP: u8 = 0x5D;

/// Stands in for the parent's escalate channel: answers each op from a table
/// and records which ops were asked, so a test can prove nothing escalated.
const RECORDING_PARENT_SOURCE: &std::ffi::CStr = c"
class RecordingParent:
    def __init__(self, answers):
        self.answers = answers
        self.ops_asked = []

    def __call__(self, op):
        self.ops_asked.append(op['op'])
        if op['op'] not in self.answers:
            raise RuntimeError('this fake parent has no answer for ' + op['op'])
        return self.answers[op['op']]
";

fn recording_parent_answering(python: Python<'_>, answers: &Bound<'_, PyDict>) -> Py<PyAny> {
    PyModule::from_code(
        python,
        RECORDING_PARENT_SOURCE,
        c"recording_parent.py",
        c"recording_parent",
    )
    .expect("the fake parent's source compiles")
    .getattr("RecordingParent")
    .expect("the module defines it")
    .call1((answers,))
    .expect("the fake parent constructs")
    .unbind()
}

fn ops_the_parent_was_asked(python: Python<'_>, parent: &Py<PyAny>) -> Vec<String> {
    parent
        .bind(python)
        .getattr("ops_asked")
        .expect("the fake parent records its ops")
        .extract()
        .expect("op names are strings")
}

/// A host-visible OPAQUE_FD staging the engine has copied a frame into,
/// registered with the service the way the engine's surface store
/// registers one — the staging fd, then the copy timeline's when one
/// crosses. The returned connection holds the registration alive.
fn register_a_copied_staging(
    share: &SurfaceShareUnderTest,
    host_device: &std::sync::Arc<HostVulkanDevice>,
    staging_share_id: &str,
    copy_done: Option<&HostVulkanTimelineSemaphore>,
) -> (HostVulkanBuffer, UnixStream) {
    let staging =
        HostVulkanBuffer::new_opaque_fd_export_host_cached(host_device, STAGING_BYTE_SIZE)
            .expect("a host-visible OPAQUE_FD staging");
    // SAFETY: the staging is a live mapping of STAGING_BYTE_SIZE bytes.
    unsafe {
        std::ptr::write_bytes(
            staging.mapped_ptr(),
            ENGINE_COPIED_STAMP,
            STAGING_BYTE_SIZE as usize,
        )
    };
    let memory_type_index = staging
        .vma_allocation_memory_type_index()
        .expect("a VMA allocation states its memory type");
    // SAFETY: each export mints a fresh fd this test owns.
    let mut registered_fds = vec![unsafe {
        OwnedFd::from_raw_fd(
            staging
                .export_opaque_fd_memory()
                .expect("the staging exports"),
        )
    }];
    if let Some(copy_done) = copy_done {
        // SAFETY: as above.
        registered_fds.push(unsafe {
            OwnedFd::from_raw_fd(copy_done.export_opaque_fd().expect("the timeline exports"))
        });
    }
    let raw_registered_fds: Vec<i32> = registered_fds.iter().map(|fd| fd.as_raw_fd()).collect();

    let publisher = streamlib_surface_client::connect_to_surface_share_socket(&share.socket_path)
        .expect("a publisher connection");
    let (response, _no_reply_fds) = streamlib_surface_client::send_request_with_fds(
        &publisher,
        &serde_json::json!({
            "op": "register",
            "surface_id": staging_share_id,
            "runtime_id": "engine:export-staging-check-out-under-test",
            "width": STAGING_WIDTH,
            "height": STAGING_HEIGHT,
            "format": PixelFormat::Rgba32.wire_name(),
            "resource_type": "pixel_buffer",
            "handle_type": "opaque_fd",
            "plane_sizes": [STAGING_BYTE_SIZE],
            "plane_offsets": [0],
            "has_produce_done_fd": copy_done.is_some(),
            "has_consume_done_fd": false,
            "vk_memory_type_index": memory_type_index,
        }),
        &raw_registered_fds,
        0,
    )
    .expect("the staging registers");
    assert!(
        response.get("error").is_none(),
        "the service refused the staging: {response}"
    );
    (staging, publisher)
}

/// The parent's answers for a CPU-readback staging opened and copied once,
/// the copy answering `answered_timeline_value`.
fn readback_answers<'py>(
    python: Python<'py>,
    staging_share_id: &str,
    host_device: &HostVulkanDevice,
    answered_timeline_value: Option<&str>,
) -> Bound<'py, PyDict> {
    let opened = PyDict::new(python);
    opened.set_item("handle_id", staging_share_id).unwrap();
    opened.set_item("width", STAGING_WIDTH).unwrap();
    opened.set_item("height", STAGING_HEIGHT).unwrap();
    opened
        .set_item("format", PixelFormat::Rgba32.wire_name())
        .unwrap();
    opened
        .set_item("staging_byte_size", STAGING_BYTE_SIZE.to_string())
        .unwrap();
    opened
        .set_item(
            "bytes_per_row",
            (STAGING_BYTE_SIZE / u64::from(STAGING_HEIGHT)).to_string(),
        )
        .unwrap();
    opened.set_item("writable", false).unwrap();
    opened
        .set_item(
            "exporting_device_uuid",
            streamlib_consumer_rhi::lowercase_hex_of_vulkan_device_uuid(
                host_device.physical_device_uuid(),
            ),
        )
        .unwrap();
    let copied = PyDict::new(python);
    copied.set_item("handle_id", staging_share_id).unwrap();
    if let Some(answered_timeline_value) = answered_timeline_value {
        copied
            .set_item("timeline_value", answered_timeline_value)
            .unwrap();
    }
    let answers = PyDict::new(python);
    answers
        .set_item("open_cpu_readback_staging", opened)
        .unwrap();
    answers.set_item("run_cpu_readback_copy", copied).unwrap();
    answers
}

fn host_device_or_skip() -> Option<std::sync::Arc<HostVulkanDevice>> {
    match HostVulkanDevice::new() {
        Ok(host_device) => Some(host_device),
        Err(unavailable) => {
            tracing::warn!("skipping: no host Vulkan device ({unavailable})");
            None
        }
    }
}

/// On a device that exports no timeline the staging is registered alone and
/// the copy's reply carries no value: the check-out takes the one fd, the
/// copy waits on nothing, and the mapping holds the engine's frame the
/// moment the reply lands. Needs a Vulkan device; skips where none comes up.
#[test]
fn a_staging_without_a_timeline_reads_after_the_reply() {
    let Some(host_device) = host_device_or_skip() else {
        return;
    };
    let share = SurfaceShareUnderTest::start("staging-no-timeline");
    let staging_share_id = "staging-without-a-timeline-under-test";
    let (_staging, _held_registration) =
        register_a_copied_staging(&share, &host_device, staging_share_id, None);

    Python::initialize();
    let parent = Python::attach(|python| {
        let answers = readback_answers(python, staging_share_id, &host_device, None);
        recording_parent_answering(python, &answers)
    });
    let exchange_client = share.exchange_client_labelled_importing_on_answered_by(
        "staging-no-timeline",
        Ok(host_device.physical_device_uuid()),
        Python::attach(|python| parent.clone_ref(python)),
    );

    Python::attach(|python| {
        let export = exchange_client
            .open_cpu_readback_export(python, staging_share_id)
            .expect("a staging registered without a timeline checks out on its one fd");
        assert!(
            export.copy_done.is_none(),
            "no timeline was registered, so none may be imported"
        );
        exchange_client
            .run_cpu_readback_copy(
                python,
                CpuReadbackCopyDirection::SurfaceIntoStaging,
                staging_share_id,
                &export,
            )
            .expect("a copy answering no timeline value waits on nothing");
        // SAFETY: the import maps the staging's STAGING_BYTE_SIZE bytes.
        let read_after_the_reply = unsafe {
            std::slice::from_raw_parts(
                export.consumer_buffer.mapped_ptr(),
                STAGING_BYTE_SIZE as usize,
            )
        };
        assert!(
            read_after_the_reply
                .iter()
                .all(|byte| *byte == ENGINE_COPIED_STAMP),
            "the helper must map the engine's staging, not a fresh allocation"
        );
        assert_eq!(
            ops_the_parent_was_asked(python, &parent),
            ["open_cpu_readback_staging", "run_cpu_readback_copy"]
        );
    });
}

/// A copy answering a timeline value over a staging checked out with no
/// timeline is a parent the check-out disagrees with: refused by name,
/// never read past.
#[test]
fn a_timeline_value_answered_for_a_staging_checked_out_without_one_is_refused() {
    let Some(host_device) = host_device_or_skip() else {
        return;
    };
    let share = SurfaceShareUnderTest::start("staging-stray-value");
    let staging_share_id = "staging-answered-a-stray-value-under-test";
    let (_staging, _held_registration) =
        register_a_copied_staging(&share, &host_device, staging_share_id, None);

    Python::initialize();
    let parent = Python::attach(|python| {
        let answers = readback_answers(python, staging_share_id, &host_device, Some("1"));
        recording_parent_answering(python, &answers)
    });
    let exchange_client = share.exchange_client_labelled_importing_on_answered_by(
        "staging-stray-value",
        Ok(host_device.physical_device_uuid()),
        parent,
    );

    Python::attach(|python| {
        let export = exchange_client
            .open_cpu_readback_export(python, staging_share_id)
            .expect("the staging checks out");
        let refusal = exchange_client
            .run_cpu_readback_copy(
                python,
                CpuReadbackCopyDirection::SurfaceIntoStaging,
                staging_share_id,
                &export,
            )
            .expect_err("a value with no timeline to wait on must be refused")
            .to_string();
        assert!(
            refusal.contains("checked out with no timeline") && refusal.contains(staging_share_id),
            "the refusal names the disagreement and the surface: {refusal}"
        );
    });
}

/// Where the device exports a timeline the staging crosses with it, the
/// check-out imports it, and the copy waits for the answered value — and a
/// reply that leaves the value out is refused rather than read past. Skips
/// on a device that exports no timeline, which has no such registration.
#[test]
fn a_staging_with_its_timeline_waits_for_the_answered_value_and_refuses_a_reply_without_one() {
    let Some(host_device) = host_device_or_skip() else {
        return;
    };
    if !host_device.supports_semaphore_export() {
        tracing::warn!(
            "skipping: {} exports no timeline semaphore, so no staging crosses with one",
            host_device.name()
        );
        return;
    }
    let copy_done = HostVulkanTimelineSemaphore::new_exportable(host_device.device(), 0)
        .expect("a device that exports a timeline mints one");
    copy_done.signal_host(1).expect("the copy signals value 1");
    let share = SurfaceShareUnderTest::start("staging-with-timeline");

    for answered_timeline_value in [Some("1"), None] {
        let staging_share_id = match answered_timeline_value {
            Some(_) => "staging-with-a-timeline-answered-under-test",
            None => "staging-with-a-timeline-unanswered-under-test",
        };
        let (_staging, _held_registration) =
            register_a_copied_staging(&share, &host_device, staging_share_id, Some(&copy_done));
        Python::initialize();
        let parent = Python::attach(|python| {
            let answers = readback_answers(
                python,
                staging_share_id,
                &host_device,
                answered_timeline_value,
            );
            recording_parent_answering(python, &answers)
        });
        let exchange_client = share.exchange_client_labelled_importing_on_answered_by(
            staging_share_id,
            Ok(host_device.physical_device_uuid()),
            parent,
        );

        Python::attach(|python| {
            let export = exchange_client
                .open_cpu_readback_export(python, staging_share_id)
                .expect("a staging registered with its timeline checks out on two fds");
            assert!(
                export.copy_done.is_some(),
                "the registered timeline is imported"
            );
            let copied = exchange_client.run_cpu_readback_copy(
                python,
                CpuReadbackCopyDirection::SurfaceIntoStaging,
                staging_share_id,
                &export,
            );
            match answered_timeline_value {
                Some(_) => copied.expect("the answered value was already signalled"),
                None => {
                    let refusal = copied
                        .expect_err("a reply with no value over an imported timeline")
                        .to_string();
                    assert!(
                        refusal.contains("answered no timeline value"),
                        "the refusal names the missing value: {refusal}"
                    );
                }
            }
        });
    }
}

/// The device-tensor doors all open the device export, and on a software
/// device it refuses by the device's name before the parent is asked for
/// anything — no staging opens for a tensor no GPU package can import. On a
/// hardware device the same call goes on to ask the parent.
#[test]
fn as_device_tensor_refuses_on_a_software_device_naming_it_before_any_staging_opens() {
    let Some(host_device) = host_device_or_skip() else {
        return;
    };
    let share = SurfaceShareUnderTest::start("device-export-software");
    Python::initialize();
    let parent = Python::attach(|python| recording_parent_answering(python, &PyDict::new(python)));
    let exchange_client = share.exchange_client_labelled_importing_on_answered_by(
        "device-export-software",
        Ok(host_device.physical_device_uuid()),
        Python::attach(|python| parent.clone_ref(python)),
    );

    Python::attach(|python| {
        let refusal = exchange_client
            .open_device_export(python, "frame-under-test")
            .err()
            .expect("the fake parent answers nothing, so no device export can open")
            .to_string();
        let ops_asked = ops_the_parent_was_asked(python, &parent);
        if host_device.device_is_software() {
            assert!(
                refusal.contains(&host_device.name()) && refusal.contains("software"),
                "the refusal names the software device: {refusal}"
            );
            assert!(
                ops_asked.is_empty(),
                "nothing may escalate on a software device, yet the parent was asked {ops_asked:?}"
            );
        } else {
            assert!(
                !refusal.contains("software Vulkan device"),
                "{} is a hardware device and must not be refused as software: {refusal}",
                host_device.name()
            );
            assert_eq!(ops_asked, ["open_device_export_staging"]);
        }
    });
}

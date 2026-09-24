// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A texture crossing to a helper process on macOS, on an IOSurface.
//!
//! The engine allocates an image over a private IOSurface and registers it
//! whole with its surface store; a spawned helper checks it out over the Mach
//! channel, imports the surface as an image and the timeline pair as shared
//! events in its own Vulkan device, waits for the engine's GPU to signal the
//! write done, and reads the pixels back. Needs MoltenVK on a real GPU.

#![cfg(target_os = "macos")]

#[path = "support/iosurface_texture_test_pattern.rs"]
mod iosurface_texture_test_pattern;

use std::io::BufRead as _;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use iosurface_texture_test_pattern::engine_pattern_byte;
use streamlib_consumer_rhi::VulkanLayout;
use streamlib_engine::apple_surface_share::{
    ConsumerReleaseOutcome, CrossProcessTimelinePair, IOSurfaceShareState, MachSurfaceShareService,
};
use streamlib_engine::core::context::{GpuContext, SurfaceStore};
use streamlib_engine::core::rhi::{Texture, TextureDescriptor, TextureFormat, TextureUsages};
use streamlib_engine::host_rhi::{
    HostVulkanBuffer, HostVulkanTimelineSemaphore, ImageCopyRegion, RhiCommandRecorder,
    VulkanAccess, VulkanStage,
};
use streamlib_engine::{HostGpuDeviceExt, HostSurfaceStoreExt};
use streamlib_surface_client::SURFACE_SHARE_MACH_SERVICE_ENVIRONMENT_VARIABLE;

const HELPER_BINARY: &str = env!("CARGO_BIN_EXE_iosurface_texture_helper");

/// Generous: a helper's MoltenVK device is ~0.5 s cold.
const HELPER_EVENT_BUDGET: Duration = Duration::from_secs(30);

/// An odd width, so a surface that pads its rows reads wrong at the packed
/// stride; large enough that the GPU copy outlasts a read that does not wait.
const TEXTURE_WIDTH: u32 = 2047;
const TEXTURE_HEIGHT: u32 = 1024;
const TEXTURE_BYTES: u64 = TEXTURE_WIDTH as u64 * TEXTURE_HEIGHT as u64 * 4;

/// An engine with one IOSurface-backed texture registered, through its
/// surface store, under a surface id with its timeline pair. Fields drop in
/// order, so the GPU context — and the device every timeline was made on —
/// goes last.
struct EngineWithOneRegisteredTexture {
    service: MachSurfaceShareService,
    state: IOSurfaceShareState,
    surface_id: String,
    pair: Arc<CrossProcessTimelinePair>,
    texture: Texture,
    staging: HostVulkanBuffer,
    recorder: RhiCommandRecorder,
    _store: SurfaceStore,
    _gpu: GpuContext,
}

impl EngineWithOneRegisteredTexture {
    /// `None` when this machine has no GPU to run on.
    fn start(label: &str) -> Option<Self> {
        let gpu = match GpuContext::init_for_platform() {
            Ok(gpu) => gpu,
            Err(unavailable) => {
                tracing::warn!("skipping — no GPU: {unavailable}");
                return None;
            }
        };
        let device = Arc::clone(gpu.device().vulkan_device());
        let texture = gpu
            .device()
            .create_texture_iosurface_backed(
                &TextureDescriptor::new(TEXTURE_WIDTH, TEXTURE_HEIGHT, TextureFormat::Rgba8Unorm)
                    .with_usage(
                        TextureUsages::COPY_SRC
                            | TextureUsages::COPY_DST
                            | TextureUsages::TEXTURE_BINDING
                            | TextureUsages::STORAGE_BINDING,
                    ),
            )
            .expect("an IOSurface-backed texture");
        let timeline = || {
            Arc::new(
                HostVulkanTimelineSemaphore::new_exportable(device.device(), 0)
                    .expect("an exportable timeline"),
            )
        };
        let pair = Arc::new(CrossProcessTimelinePair::new(timeline(), timeline()));

        let state = IOSurfaceShareState::new();
        let mut service = MachSurfaceShareService::new(
            state.clone(),
            format!(
                "com.tatolab.streamlib.iosurface-texture-test.{label}.{}",
                std::process::id()
            ),
        );
        service.start().expect("the service starts");
        let store = SurfaceStore::new_sharing_the_mach_services_tables(
            service.service_name().to_string(),
            "R-engine".to_string(),
            Arc::clone(state.check_out_leases()),
            Arc::clone(state.cross_process_timeline_pairs()),
        );
        store
            .connect()
            .expect("the store connects to its own service");
        let surface_id = format!("texture-{label}");
        store
            .register_texture_with_timeline_pair(
                &surface_id,
                &texture,
                &pair,
                VulkanLayout::UNDEFINED,
            )
            .expect("the texture registers whole");

        Some(Self {
            service,
            state,
            surface_id,
            pair,
            texture,
            staging: HostVulkanBuffer::new_storage_buffer_host_visible(&device, TEXTURE_BYTES)
                .expect("a staging buffer"),
            recorder: RhiCommandRecorder::new(&device, "iosurface-texture-test")
                .expect("a recorder"),
            _store: store,
            _gpu: gpu,
        })
    }

    /// Signal `produce_done` at 1 from the engine's GPU queue, after copying
    /// the pattern into the texture first when `write_the_pattern`.
    fn produce_on_the_gpu(&mut self, write_the_pattern: bool) {
        self.recorder.begin().expect("begin");
        if write_the_pattern {
            let pattern: Vec<u8> = (0..TEXTURE_BYTES as usize)
                .map(engine_pattern_byte)
                .collect();
            // SAFETY: nothing reads the staging buffer until this submit.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    pattern.as_ptr(),
                    self.staging.mapped_ptr(),
                    pattern.len(),
                );
            }
            self.recorder
                .record_image_barrier(
                    &self.texture,
                    VulkanLayout::UNDEFINED,
                    VulkanLayout::TRANSFER_DST_OPTIMAL,
                    VulkanStage::NONE,
                    VulkanStage::COPY,
                    VulkanAccess::NONE,
                    VulkanAccess::TRANSFER_WRITE,
                )
                .expect("to transfer-dst");
            self.recorder
                .record_copy_buffer_to_image(
                    &self.staging,
                    &self.texture,
                    VulkanLayout::TRANSFER_DST_OPTIMAL,
                    ImageCopyRegion::tightly_packed(TEXTURE_WIDTH, TEXTURE_HEIGHT),
                )
                .expect("record the copy");
            self.recorder
                .record_image_barrier(
                    &self.texture,
                    VulkanLayout::TRANSFER_DST_OPTIMAL,
                    VulkanLayout::GENERAL,
                    VulkanStage::COPY,
                    VulkanStage::ALL_COMMANDS,
                    VulkanAccess::TRANSFER_WRITE,
                    VulkanAccess::MEMORY_READ,
                )
                .expect("to general");
        }
        self.recorder
            .submit_signaling_timeline(self.pair.produce_done(), 1)
            .expect("the engine's GPU signals produce_done");
    }
}

struct SpawnedHelperProcess {
    child: Child,
    stdout_lines: mpsc::Receiver<Option<String>>,
}

impl SpawnedHelperProcess {
    fn read_back(engine: &EngineWithOneRegisteredTexture) -> Self {
        let mut child = Command::new(HELPER_BINARY)
            .args(["read-back", &engine.surface_id])
            .env(
                SURFACE_SHARE_MACH_SERVICE_ENVIRONMENT_VARIABLE,
                engine.service.service_name(),
            )
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn the helper");
        let stdout = child.stdout.take().expect("the helper's stdout");
        let (line_sender, stdout_lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout)
                .lines()
                .map_while(Result::ok)
            {
                let _ = line_sender.send(Some(line));
            }
            let _ = line_sender.send(None);
        });
        Self {
            child,
            stdout_lines,
        }
    }

    fn next_line(&self) -> String {
        self.stdout_lines
            .recv_timeout(HELPER_EVENT_BUDGET)
            .expect("the helper reported within the budget")
            .expect("the helper reported before exiting")
    }

    fn wait_for_exit(mut self) -> std::process::ExitStatus {
        self.child.wait().expect("reap the helper")
    }
}

/// Run the helper against one registered texture, the engine producing on
/// its GPU only once the helper has imported everything; answer the helper's
/// `READ` line.
fn read_back_through_a_helper(label: &str, write_the_pattern: bool) -> Option<String> {
    let mut engine = EngineWithOneRegisteredTexture::start(label)?;
    let helper = SpawnedHelperProcess::read_back(&engine);
    let _admission = engine
        .service
        .rendezvous()
        .admit_helper_process(helper.child.id());

    let imported = helper.next_line();
    assert!(
        imported.starts_with("IMPORTED"),
        "the helper did not import the texture: {imported}"
    );
    assert!(
        imported.contains("tiling=0"),
        "the recipe did not carry OPTIMAL: {imported}"
    );
    engine.produce_on_the_gpu(write_the_pattern);

    let read = helper.next_line();
    assert_eq!(helper.next_line(), "RELEASED");
    assert!(matches!(
        engine
            .pair
            .wait_for_consumer_release(1)
            .expect("the engine's wait on the helper's release"),
        ConsumerReleaseOutcome::Released
    ));
    assert!(helper.wait_for_exit().success());
    engine
        .recorder
        .wait_for_completion()
        .expect("the copy completes");
    assert!(
        engine.state.registration_of(&engine.surface_id).is_some(),
        "the helper's exit took the engine's own registration"
    );
    Some(read)
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — needs MoltenVK on a GPU; set --features hardware-tests"
)]
#[test]
fn a_helper_reads_back_the_pixels_the_engine_wrote_into_an_iosurface_texture() {
    let Some(read) = read_back_through_a_helper("written", true) else {
        return;
    };
    assert_eq!(read, "READ mismatches=0");
}

/// The negative control: the same run with the engine's write left out reads
/// back something else, so the pixel check above can fail.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — needs MoltenVK on a GPU; set --features hardware-tests"
)]
#[test]
fn without_the_engines_write_the_helper_does_not_read_the_pattern() {
    let Some(read) = read_back_through_a_helper("unwritten", false) else {
        return;
    };
    assert_ne!(read, "READ mismatches=0");
}

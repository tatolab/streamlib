// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Polyglot continuous processor reference example (issue #542).
//!
//! Exercises `execution: continuous` end-to-end through both the
//! Python and Deno SDKs after the subprocess runner's continuous-mode
//! dispatch was reworked from `time.sleep` / `setTimeout` to a real
//! `MonotonicTimer` (timerfd, drift-free).
//!
//! The polyglot processor's `process()` is called by the runner once
//! per tick at the manifest's `interval_ms`. Each call records
//! `monotonic_now_ns()` and writes (tick_count, first_tick_ns,
//! last_tick_ns) into a host-pre-registered cpu-readback surface.
//! After the runtime stops, this binary reads those bytes back and
//! asserts:
//!
//! 1. Tick count is in the expected range — "process() actually got
//!    called the right number of times for the run length."
//! 2. Average inter-tick interval, derived from
//!    `(last_tick_ns - first_tick_ns) / (tick_count - 1)`, is within
//!    a slack window of the manifest's nominal interval — "the
//!    timerfd is pacing correctly, not bursty / not stalling."
//!
//! Failure of (2) is the regression-detection signal that would fire
//! if someone reverted to `time.sleep` semantics.
//!
//! Build the Python `.slpkg` first:
//!   cargo run -p streamlib-cli -- pack examples/polyglot-continuous-processor/python
//!
//! Run:
//!   cargo run -p polyglot-continuous-processor-scenario -- --runtime=python
//!   cargo run -p polyglot-continuous-processor-scenario -- --runtime=deno

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use streamlib::core::context::{
    CpuReadbackBridge, CpuReadbackCopyDirection, GpuContext,
};
use streamlib::core::rhi::{PixelFormat, RhiPixelBuffer, TextureFormat};
use streamlib::core::StreamError;
use streamlib::host_rhi::{
    HostMarker, HostVulkanPixelBuffer, HostVulkanTimelineSemaphore,
};
use streamlib::{ProcessorSpec, Result, StreamRuntime};
use streamlib_adapter_abi::{
    StreamlibSurface, SurfaceFormat, SurfaceId, SurfaceSyncState,
    SurfaceTransportHandle, SurfaceUsage,
};
use streamlib_adapter_cpu_readback::{
    CpuReadbackCopyTrigger, CpuReadbackSurfaceAdapter, HostSurfaceRegistration,
    InProcessCpuReadbackCopyTrigger, VulkanLayout,
};

const SCENARIO_SURFACE_ID: SurfaceId = 1;
const SURFACE_SIZE: u32 = 64;
const RUN_DURATION: Duration = Duration::from_secs(2);
/// Manifest-declared interval. Must match the YAML in
/// {python,deno}/streamlib.yaml — change both if changing.
const NOMINAL_INTERVAL_MS: u32 = 16;
/// Allow ±10ms of slack on the average inter-tick interval. Way wider
/// than timerfd's natural resolution; keeps the gate robust against
/// CI scheduler noise + cpu-readback round-trip overhead.
const INTERVAL_SLACK_MS: f64 = 10.0;
const MIN_TICK_COUNT: u32 = 30;
const MAX_TICK_COUNT: u32 = 400; // generous upper bound — runner shouldn't burst

type HostAdapter =
    CpuReadbackSurfaceAdapter<streamlib::host_rhi::HostVulkanDevice>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RuntimeKind {
    Python,
    Deno,
}

impl RuntimeKind {
    fn parse(s: &str) -> std::result::Result<Self, String> {
        match s {
            "python" => Ok(Self::Python),
            "deno" => Ok(Self::Deno),
            other => Err(format!(
                "unknown --runtime value '{other}' (expected 'python' or 'deno')"
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::Deno => "deno",
        }
    }

    fn processor_name(self) -> &'static str {
        match self {
            Self::Python => "com.tatolab.polyglot_continuous_processor",
            Self::Deno => "com.tatolab.polyglot_continuous_processor_deno",
        }
    }
}

#[derive(Debug)]
struct TickReport {
    count: u32,
    first_ns: u64,
    last_ns: u64,
}

impl TickReport {
    fn average_interval_ns(&self) -> Option<f64> {
        if self.count < 2 {
            return None;
        }
        let span = self.last_ns.saturating_sub(self.first_ns);
        Some(span as f64 / (self.count as f64 - 1.0))
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(report) => {
            let avg_ns = report.average_interval_ns();
            println!(
                "✓ ticks={} avg_interval_ms={}",
                report.count,
                avg_ns
                    .map(|ns| format!("{:.3}", ns / 1_000_000.0))
                    .unwrap_or_else(|| "n/a".into()),
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("✗ scenario failed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<TickReport> {
    let mut runtime_kind = RuntimeKind::Python;
    for a in std::env::args().skip(1) {
        if let Some(value) = a.strip_prefix("--runtime=") {
            runtime_kind =
                RuntimeKind::parse(value).map_err(StreamError::Configuration)?;
        }
    }

    println!("=== Polyglot continuous processor scenario (#542) ===");
    println!("Runtime:           {}", runtime_kind.as_str());
    println!("Surface:           {SURFACE_SIZE}x{SURFACE_SIZE} BGRA8 (id {SCENARIO_SURFACE_ID})");
    println!("Nominal interval:  {NOMINAL_INTERVAL_MS}ms");
    println!("Run length:        {:?}", RUN_DURATION);

    let runtime = StreamRuntime::new()?;
    let adapter_slot: Arc<Mutex<Option<Arc<HostAdapter>>>> =
        Arc::new(Mutex::new(None));

    {
        let adapter_slot = Arc::clone(&adapter_slot);
        runtime.install_setup_hook(move |gpu| {
            let host_device = Arc::clone(gpu.device().vulkan_device());
            let trigger = Arc::new(InProcessCpuReadbackCopyTrigger::new(
                Arc::clone(&host_device),
            ))
                as Arc<dyn CpuReadbackCopyTrigger<HostMarker>>;
            let adapter: Arc<HostAdapter> = Arc::new(
                CpuReadbackSurfaceAdapter::new(Arc::clone(&host_device), trigger),
            );
            register_host_surface(&adapter, gpu)
                .map_err(StreamError::Configuration)?;
            gpu.set_cpu_readback_bridge(Arc::new(BridgeImpl {
                adapter: Arc::clone(&adapter),
            }));
            *adapter_slot.lock().unwrap() = Some(adapter);
            println!("✓ cpu-readback adapter registered, bridge installed");
            Ok(())
        });
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    match runtime_kind {
        RuntimeKind::Python => {
            let slpkg_path = manifest_dir
                .join("python/polyglot-continuous-processor-0.1.0.slpkg");
            let project_path = manifest_dir.join("python");
            if slpkg_path.exists() {
                runtime.load_package(&slpkg_path)?;
            } else {
                runtime.load_project(&project_path)?;
            }
        }
        RuntimeKind::Deno => {
            runtime.load_project(&manifest_dir.join("deno"))?;
        }
    }

    let processor = runtime.add_processor(ProcessorSpec::new(
        runtime_kind.processor_name(),
        serde_json::json!({
            "cpu_readback_surface_id": SCENARIO_SURFACE_ID,
        }),
    ))?;
    println!("+ ContinuousProcessor: {processor}");

    runtime.start()?;
    std::thread::sleep(RUN_DURATION);
    runtime.stop()?;

    let adapter = adapter_slot
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| StreamError::Runtime("setup hook never ran".into()))?;
    let report = read_tick_report(&adapter)?;

    if report.count < MIN_TICK_COUNT || report.count > MAX_TICK_COUNT {
        return Err(StreamError::Runtime(format!(
            "tick count {} outside expected [{MIN_TICK_COUNT}, {MAX_TICK_COUNT}]",
            report.count,
        )));
    }
    if let Some(avg_ns) = report.average_interval_ns() {
        let nominal_ns = (NOMINAL_INTERVAL_MS as f64) * 1_000_000.0;
        let slack_ns = INTERVAL_SLACK_MS * 1_000_000.0;
        if (avg_ns - nominal_ns).abs() > slack_ns {
            return Err(StreamError::Runtime(format!(
                "average inter-tick interval {:.3}ms outside nominal {NOMINAL_INTERVAL_MS}ms ± {INTERVAL_SLACK_MS}ms",
                avg_ns / 1_000_000.0,
            )));
        }
    }
    Ok(report)
}

struct BridgeImpl {
    adapter: Arc<HostAdapter>,
}

impl CpuReadbackBridge for BridgeImpl {
    fn run_copy(
        &self,
        surface_id: SurfaceId,
        direction: CpuReadbackCopyDirection,
    ) -> std::result::Result<u64, String> {
        match direction {
            CpuReadbackCopyDirection::ImageToBuffer => self
                .adapter
                .run_bridge_copy_image_to_buffer(surface_id)
                .map_err(|e| format!("{e:?}")),
            CpuReadbackCopyDirection::BufferToImage => self
                .adapter
                .run_bridge_copy_buffer_to_image(surface_id)
                .map_err(|e| format!("{e:?}")),
        }
    }

    fn try_run_copy(
        &self,
        surface_id: SurfaceId,
        direction: CpuReadbackCopyDirection,
    ) -> std::result::Result<Option<u64>, String> {
        Ok(Some(self.run_copy(surface_id, direction)?))
    }
}

fn register_host_surface(
    adapter: &Arc<HostAdapter>,
    gpu: &GpuContext,
) -> std::result::Result<(), String> {
    let host_device = adapter.device();
    let stream_texture = gpu
        .acquire_render_target_dma_buf_image(
            SURFACE_SIZE,
            SURFACE_SIZE,
            TextureFormat::Bgra8Unorm,
        )
        .map_err(|e| format!("acquire_render_target_dma_buf_image: {e}"))?;
    let texture_arc = Arc::clone(stream_texture.vulkan_inner());

    let staging = HostVulkanPixelBuffer::new(
        host_device,
        SURFACE_SIZE,
        SURFACE_SIZE,
        4,
        PixelFormat::Bgra32,
    )
    .map_err(|e| format!("HostVulkanPixelBuffer::new: {e}"))?;
    let staging_arc = Arc::new(staging);
    let staging_rhi =
        RhiPixelBuffer::from_host_vulkan_pixel_buffer(Arc::clone(&staging_arc));

    let timeline = Arc::new(
        HostVulkanTimelineSemaphore::new_exportable(host_device.device(), 0)
            .map_err(|e| format!("HostVulkanTimelineSemaphore: {e}"))?,
    );

    let surface_store = gpu
        .surface_store()
        .ok_or_else(|| "GpuContext has no surface_store".to_string())?;
    surface_store
        .register_pixel_buffer_with_timeline(
            &SCENARIO_SURFACE_ID.to_string(),
            &staging_rhi,
            Some(timeline.as_ref()),
        )
        .map_err(|e| format!("register_pixel_buffer_with_timeline: {e}"))?;

    adapter
        .register_host_surface(
            SCENARIO_SURFACE_ID,
            HostSurfaceRegistration::<HostMarker> {
                texture: Some(texture_arc),
                staging_planes: vec![staging_arc],
                timeline,
                initial_image_layout: VulkanLayout::UNDEFINED,
                format: SurfaceFormat::Bgra8,
                width: SURFACE_SIZE,
                height: SURFACE_SIZE,
            },
        )
        .map_err(|e| format!("register_host_surface: {e:?}"))?;
    Ok(())
}

fn read_tick_report(adapter: &Arc<HostAdapter>) -> Result<TickReport> {
    use streamlib_adapter_abi::SurfaceAdapter;

    let surface = StreamlibSurface::new(
        SCENARIO_SURFACE_ID,
        SURFACE_SIZE,
        SURFACE_SIZE,
        SurfaceFormat::Bgra8,
        SurfaceUsage::CPU_READBACK,
        SurfaceTransportHandle::empty(),
        SurfaceSyncState::default(),
    );
    let guard = adapter.acquire_read(&surface).map_err(|e| {
        StreamError::Runtime(format!("acquire_read for read-back: {e:?}"))
    })?;
    let view = guard.view();
    let plane = view.plane(0);
    let bytes = plane.bytes();
    if bytes.len() < 24 {
        return Err(StreamError::Runtime(format!(
            "surface plane too small: {} bytes",
            bytes.len()
        )));
    }
    let count = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let first_ns = u64::from_le_bytes([
        bytes[8], bytes[9], bytes[10], bytes[11],
        bytes[12], bytes[13], bytes[14], bytes[15],
    ]);
    let last_ns = u64::from_le_bytes([
        bytes[16], bytes[17], bytes[18], bytes[19],
        bytes[20], bytes[21], bytes[22], bytes[23],
    ]);
    Ok(TickReport { count, first_ns, last_ns })
}

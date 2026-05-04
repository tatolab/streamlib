// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Polyglot Vulkan adapter ray-tracing scenario (#667).
//!
//! End-to-end gate for the subprocess `VulkanContext.dispatch_ray_tracing`
//! path: the host pre-allocates ONE storage-image-capable
//! `HostVulkanTexture` (transitioned to `GENERAL`), registers it with
//! surface-share AND with `GpuContext::register_texture_with_layout`
//! under a known UUID, and installs a `RayTracingKernelBridge` wired to
//! its `VulkanRayTracingKernel` + `VulkanAccelerationStructure`. A
//! Python or Deno polyglot processor receives a trigger, builds a
//! single-triangle BLAS + identity TLAS via escalate IPC's
//! `register_acceleration_structure_blas` / `_tlas`, registers the RT
//! kernel via `register_ray_tracing_kernel`, and dispatches a trace via
//! `run_ray_tracing_kernel` — which routes through the host's
//! `VulkanRayTracingKernel::trace_rays`. This binary then reads the
//! storage image back via Vulkan and writes a PNG; reading the PNG
//! with the Read tool is the visual gate.
//!
//! The shaders (`shaders/scene.{rgen,rmiss,rchit}`) are compiled to
//! SPIR-V at build time via `build.rs`, embedded as bytes here, and
//! shipped to the polyglot processor via the processor config as hex
//! strings. The `variant` push constant flips the miss-shader gradient
//! palette so Python (variant 0) and Deno (variant 1) PNGs are
//! visually distinct.
//!
//! Build the Python `.slpkg` first:
//!   cargo run -p streamlib-cli -- pack examples/polyglot-vulkan-ray-tracing/python
//!
//! Run:
//!   cargo run -p polyglot-vulkan-ray-tracing-scenario -- \
//!       --runtime=python --output=/tmp/vulkan-rt-py.png
//!   cargo run -p polyglot-vulkan-ray-tracing-scenario -- \
//!       --runtime=deno   --output=/tmp/vulkan-rt-deno.png

#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use streamlib::core::context::{
    BlasRegisterDecl, RayTracingBindingKindWire, RayTracingKernelBridge,
    RayTracingKernelRegisterDecl, RayTracingKernelRunDispatch, RayTracingShaderGroupWire,
    RayTracingShaderStageWire, TlasRegisterDecl,
};
use streamlib::core::rhi::{
    RayTracingBindingSpec, RayTracingKernelDescriptor, RayTracingPushConstants,
    RayTracingShaderGroup, RayTracingShaderStageFlags, RayTracingStage, StreamTexture,
    TextureDescriptor, TextureFormat, TextureReadbackDescriptor, TextureSourceLayout,
    TextureUsages, VulkanLayout,
};
use streamlib::core::{InputLinkPortRef, OutputLinkPortRef, StreamError};
use streamlib::host_rhi::{
    GeometryInstanceFlagsKHR, HostVulkanDevice, HostVulkanTexture, TlasInstanceDesc,
    VulkanAccelerationStructure, VulkanRayTracingKernel, VulkanTextureReadback,
};
use streamlib::{BgraFileSourceProcessor, ProcessorSpec, Result, StreamRuntime};

/// Compiled SPIR-V for the ray-generation shader.
const SCENE_RGEN_SPV: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/scene.rgen.spv"));

/// Compiled SPIR-V for the miss shader.
const SCENE_RMISS_SPV: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/scene.rmiss.spv"));

/// Compiled SPIR-V for the closest-hit shader.
const SCENE_RCHIT_SPV: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/scene.rchit.spv"));

/// UUID the host registers the storage-image surface under.
const SCENARIO_SURFACE_UUID: &str = "00000000-0000-0000-0000-0000000007a1";

/// Side length of the storage image (square; 512 is large enough to be
/// visually obvious without making the PNG huge).
const SURFACE_SIZE: u32 = 512;

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
            Self::Python => "com.tatolab.vulkan_ray_tracing",
            Self::Deno => "com.tatolab.vulkan_ray_tracing_deno",
        }
    }
}

/// Bridge between the host runtime's `set_ray_tracing_kernel_bridge`
/// and the host's `VulkanRayTracingKernel` + `VulkanAccelerationStructure`.
/// Lives in this example because the `RayTracingKernelBridge` trait
/// lives in `streamlib` and the `streamlib-adapter-vulkan` crate
/// cannot depend on the full `streamlib` (the consumer-rhi capability
/// boundary forbids it).
///
/// Holds three caches:
/// - UUID → `StreamTexture` for resolving non-AS run-time bindings to
///   host-side images (storage_image, sampled_texture).
/// - `as_id` → `Arc<VulkanAccelerationStructure>` for resolving AS
///   bindings AND for chaining BLAS → TLAS construction (TLAS instance
///   `blas_id` lookup).
/// - SHA-256 over canonical kernel descriptor bytes →
///   `Arc<VulkanRayTracingKernel>` so identical re-registration is a
///   cache hit.
struct SceneKernelBridge {
    device: Arc<HostVulkanDevice>,
    surfaces: HashMap<String, StreamTexture>,
    as_handles: parking_lot::Mutex<HashMap<String, Arc<VulkanAccelerationStructure>>>,
    kernels: parking_lot::Mutex<HashMap<String, Arc<VulkanRayTracingKernel>>>,
}

impl SceneKernelBridge {
    fn new(device: Arc<HostVulkanDevice>, surfaces: Vec<(String, StreamTexture)>) -> Self {
        Self {
            device,
            surfaces: surfaces.into_iter().collect(),
            as_handles: parking_lot::Mutex::new(HashMap::new()),
            kernels: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    fn canonical_blas_id(decl: &BlasRegisterDecl) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(b"blas|v=");
        for f in &decl.vertices {
            h.update(&f.to_le_bytes());
        }
        h.update(b"|i=");
        for i in &decl.indices {
            h.update(&i.to_le_bytes());
        }
        format!("blas-{:x}", h.finalize())
    }

    fn canonical_tlas_id(decl: &TlasRegisterDecl) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(b"tlas|n=");
        h.update(&(decl.instances.len() as u32).to_le_bytes());
        for inst in &decl.instances {
            h.update(b"|b=");
            h.update(inst.blas_id.as_bytes());
            h.update(b"|c=");
            h.update(&inst.custom_index.to_le_bytes());
            h.update(b"|m=");
            h.update(&[inst.mask]);
        }
        format!("tlas-{:x}", h.finalize())
    }

    fn canonical_kernel_id(decl: &RayTracingKernelRegisterDecl) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(b"k|s=");
        h.update(&(decl.stages.len() as u32).to_le_bytes());
        for s in &decl.stages {
            h.update(&[stage_byte(s.stage)]);
            h.update(&s.spv);
        }
        h.update(b"|g=");
        h.update(&(decl.groups.len() as u32).to_le_bytes());
        h.update(b"|nb=");
        h.update(&(decl.bindings.len() as u32).to_le_bytes());
        h.update(b"|pcs=");
        h.update(&decl.push_constant_size.to_le_bytes());
        h.update(b"|mrd=");
        h.update(&decl.max_recursion_depth.to_le_bytes());
        format!("rt-{:x}", h.finalize())
    }
}

fn stage_byte(s: RayTracingShaderStageWire) -> u8 {
    match s {
        RayTracingShaderStageWire::RayGen => 0,
        RayTracingShaderStageWire::Miss => 1,
        RayTracingShaderStageWire::ClosestHit => 2,
        RayTracingShaderStageWire::AnyHit => 3,
        RayTracingShaderStageWire::Intersection => 4,
        RayTracingShaderStageWire::Callable => 5,
    }
}

fn map_stage(s: RayTracingShaderStageWire) -> RayTracingShaderStageFlags {
    match s {
        RayTracingShaderStageWire::RayGen => RayTracingShaderStageFlags::RAYGEN,
        RayTracingShaderStageWire::Miss => RayTracingShaderStageFlags::MISS,
        RayTracingShaderStageWire::ClosestHit => RayTracingShaderStageFlags::CLOSEST_HIT,
        RayTracingShaderStageWire::AnyHit => RayTracingShaderStageFlags::ANY_HIT,
        RayTracingShaderStageWire::Intersection => RayTracingShaderStageFlags::INTERSECTION,
        RayTracingShaderStageWire::Callable => RayTracingShaderStageFlags::CALLABLE,
    }
}

fn stage_to_descriptor(s: RayTracingShaderStageWire) -> impl Fn(&[u8]) -> RayTracingStage<'_> {
    use streamlib::core::rhi::RayTracingShaderStage;
    move |spv| RayTracingStage {
        stage: match s {
            RayTracingShaderStageWire::RayGen => RayTracingShaderStage::RayGen,
            RayTracingShaderStageWire::Miss => RayTracingShaderStage::Miss,
            RayTracingShaderStageWire::ClosestHit => RayTracingShaderStage::ClosestHit,
            RayTracingShaderStageWire::AnyHit => RayTracingShaderStage::AnyHit,
            RayTracingShaderStageWire::Intersection => RayTracingShaderStage::Intersection,
            RayTracingShaderStageWire::Callable => RayTracingShaderStage::Callable,
        },
        spv,
        entry_point: "main",
    }
}

fn flags_from_bits(bits: u32) -> RayTracingShaderStageFlags {
    let mut out = RayTracingShaderStageFlags::NONE;
    if bits & 0b00_0001 != 0 {
        out |= RayTracingShaderStageFlags::RAYGEN;
    }
    if bits & 0b00_0010 != 0 {
        out |= RayTracingShaderStageFlags::MISS;
    }
    if bits & 0b00_0100 != 0 {
        out |= RayTracingShaderStageFlags::CLOSEST_HIT;
    }
    if bits & 0b00_1000 != 0 {
        out |= RayTracingShaderStageFlags::ANY_HIT;
    }
    if bits & 0b01_0000 != 0 {
        out |= RayTracingShaderStageFlags::INTERSECTION;
    }
    if bits & 0b10_0000 != 0 {
        out |= RayTracingShaderStageFlags::CALLABLE;
    }
    out
}

impl RayTracingKernelBridge for SceneKernelBridge {
    fn register_blas(
        &self,
        decl: &BlasRegisterDecl,
    ) -> std::result::Result<String, String> {
        let as_id = Self::canonical_blas_id(decl);
        let mut handles = self.as_handles.lock();
        if !handles.contains_key(&as_id) {
            let blas = VulkanAccelerationStructure::build_triangles_blas(
                &self.device,
                &as_id,
                &decl.vertices,
                &decl.indices,
            )
            .map_err(|e| format!("build_triangles_blas: {e}"))?;
            handles.insert(as_id.clone(), blas);
        }
        Ok(as_id)
    }

    fn register_tlas(
        &self,
        decl: &TlasRegisterDecl,
    ) -> std::result::Result<String, String> {
        let as_id = Self::canonical_tlas_id(decl);
        let mut handles = self.as_handles.lock();
        if !handles.contains_key(&as_id) {
            // Resolve every blas_id; build TLAS with the resolved
            // strong references (the host-RHI build_tlas keeps them
            // alive for the TLAS lifetime).
            let mut native_instances: Vec<TlasInstanceDesc> =
                Vec::with_capacity(decl.instances.len());
            for (idx, inst) in decl.instances.iter().enumerate() {
                let blas = handles.get(&inst.blas_id).cloned().ok_or_else(|| {
                    format!(
                        "TLAS instance {idx}: blas_id '{}' not registered",
                        inst.blas_id
                    )
                })?;
                native_instances.push(TlasInstanceDesc {
                    transform: inst.transform,
                    custom_index: inst.custom_index,
                    mask: inst.mask,
                    sbt_record_offset: inst.sbt_record_offset,
                    flags: vulkanalia_geometry_flags(inst.flags),
                    blas,
                });
            }
            let tlas =
                VulkanAccelerationStructure::build_tlas(&self.device, &as_id, &native_instances)
                    .map_err(|e| format!("build_tlas: {e}"))?;
            handles.insert(as_id.clone(), tlas);
        }
        Ok(as_id)
    }

    fn register_kernel(
        &self,
        decl: &RayTracingKernelRegisterDecl,
    ) -> std::result::Result<String, String> {
        let kernel_id = Self::canonical_kernel_id(decl);
        let mut kernels = self.kernels.lock();
        if !kernels.contains_key(&kernel_id) {
            // Translate wire stages to RHI RayTracingStage.
            let stages: Vec<RayTracingStage<'_>> = decl
                .stages
                .iter()
                .map(|s| (stage_to_descriptor(s.stage))(s.spv.as_slice()))
                .collect();
            // Translate wire groups to RHI RayTracingShaderGroup.
            let groups: Vec<RayTracingShaderGroup> = decl
                .groups
                .iter()
                .map(|g| match *g {
                    RayTracingShaderGroupWire::General { general_stage } => {
                        RayTracingShaderGroup::General {
                            general: general_stage,
                        }
                    }
                    RayTracingShaderGroupWire::TrianglesHit {
                        closest_hit_stage,
                        any_hit_stage,
                    } => RayTracingShaderGroup::TrianglesHit {
                        closest_hit: closest_hit_stage,
                        any_hit: any_hit_stage,
                    },
                    RayTracingShaderGroupWire::ProceduralHit {
                        intersection_stage,
                        closest_hit_stage,
                        any_hit_stage,
                    } => RayTracingShaderGroup::ProceduralHit {
                        intersection: intersection_stage,
                        closest_hit: closest_hit_stage,
                        any_hit: any_hit_stage,
                    },
                })
                .collect();
            // Translate wire bindings to RHI RayTracingBindingSpec.
            let bindings: Vec<RayTracingBindingSpec> = decl
                .bindings
                .iter()
                .map(|b| RayTracingBindingSpec {
                    binding: b.binding,
                    kind: match b.kind {
                        RayTracingBindingKindWire::StorageBuffer => {
                            streamlib::core::rhi::RayTracingBindingKind::StorageBuffer
                        }
                        RayTracingBindingKindWire::UniformBuffer => {
                            streamlib::core::rhi::RayTracingBindingKind::UniformBuffer
                        }
                        RayTracingBindingKindWire::SampledTexture => {
                            streamlib::core::rhi::RayTracingBindingKind::SampledTexture
                        }
                        RayTracingBindingKindWire::StorageImage => {
                            streamlib::core::rhi::RayTracingBindingKind::StorageImage
                        }
                        RayTracingBindingKindWire::AccelerationStructure => {
                            streamlib::core::rhi::RayTracingBindingKind::AccelerationStructure
                        }
                    },
                    stages: flags_from_bits(b.stages),
                })
                .collect();
            let push_constants = if decl.push_constant_size == 0 {
                RayTracingPushConstants::NONE
            } else {
                RayTracingPushConstants {
                    size: decl.push_constant_size,
                    stages: flags_from_bits(decl.push_constant_stages),
                }
            };
            let descriptor = RayTracingKernelDescriptor {
                label: if decl.label.is_empty() {
                    "polyglot-rt"
                } else {
                    decl.label.as_str()
                },
                stages: &stages,
                groups: &groups,
                bindings: &bindings,
                push_constants,
                max_recursion_depth: decl.max_recursion_depth,
            };
            let kernel = VulkanRayTracingKernel::new(&self.device, &descriptor)
                .map_err(|e| format!("VulkanRayTracingKernel::new: {e}"))?;
            // Suppress the unused-binding warning — `map_stage` is
            // declared so a future bridge that tracks declared vs
            // actual stages can re-use it. Drop a dummy reference.
            let _ = map_stage;
            kernels.insert(kernel_id.clone(), Arc::new(kernel));
        }
        Ok(kernel_id)
    }

    fn run_kernel(
        &self,
        dispatch: &RayTracingKernelRunDispatch,
    ) -> std::result::Result<(), String> {
        let kernel = self
            .kernels
            .lock()
            .get(&dispatch.kernel_id)
            .cloned()
            .ok_or_else(|| {
                format!(
                    "kernel_id '{}' not registered with this bridge",
                    dispatch.kernel_id
                )
            })?;

        // Bind every slot. AS bindings resolve via the as_handles map;
        // image / buffer bindings resolve via the surfaces map.
        for binding in &dispatch.bindings {
            match binding.kind {
                RayTracingBindingKindWire::AccelerationStructure => {
                    let tlas = self
                        .as_handles
                        .lock()
                        .get(&binding.target_id)
                        .cloned()
                        .ok_or_else(|| {
                            format!(
                                "binding {}: as_id '{}' not registered",
                                binding.binding, binding.target_id
                            )
                        })?;
                    kernel
                        .set_acceleration_structure(binding.binding, &tlas)
                        .map_err(|e| {
                            format!(
                                "set_acceleration_structure(binding={}): {e}",
                                binding.binding
                            )
                        })?;
                }
                RayTracingBindingKindWire::StorageImage => {
                    let texture = self.surfaces.get(&binding.target_id).ok_or_else(|| {
                        format!(
                            "binding {}: surface_uuid '{}' not registered",
                            binding.binding, binding.target_id
                        )
                    })?;
                    kernel
                        .set_storage_image(binding.binding, texture)
                        .map_err(|e| {
                            format!("set_storage_image(binding={}): {e}", binding.binding)
                        })?;
                }
                RayTracingBindingKindWire::SampledTexture => {
                    let texture = self.surfaces.get(&binding.target_id).ok_or_else(|| {
                        format!(
                            "binding {}: surface_uuid '{}' not registered",
                            binding.binding, binding.target_id
                        )
                    })?;
                    kernel
                        .set_sampled_texture(binding.binding, texture)
                        .map_err(|e| {
                            format!("set_sampled_texture(binding={}): {e}", binding.binding)
                        })?;
                }
                RayTracingBindingKindWire::StorageBuffer
                | RayTracingBindingKindWire::UniformBuffer => {
                    return Err(format!(
                        "binding {}: scenario bridge does not support buffer bindings (kind={:?})",
                        binding.binding, binding.kind
                    ));
                }
            }
        }

        if !dispatch.push_constants.is_empty() {
            kernel
                .set_push_constants(&dispatch.push_constants)
                .map_err(|e| format!("set_push_constants: {e}"))?;
        }
        kernel
            .trace_rays(dispatch.width, dispatch.height, dispatch.depth)
            .map_err(|e| format!("trace_rays: {e}"))?;
        Ok(())
    }
}

/// Map the wire `flags` bitmask onto `vk::GeometryInstanceFlagsKHR`,
/// re-exported through `host_rhi` rather than via direct vulkanalia
/// import because the example crate doesn't depend on vulkanalia
/// directly. The wire values mirror the spec bitmask exactly.
fn vulkanalia_geometry_flags(bits: u32) -> GeometryInstanceFlagsKHR {
    GeometryInstanceFlagsKHR::from_bits_truncate(bits)
}

fn main() -> Result<()> {
    let args = std::env::args().skip(1);
    let mut runtime_kind = RuntimeKind::Python;
    let mut output_png = PathBuf::from("/tmp/vulkan-rt.png");
    for a in args {
        if let Some(value) = a.strip_prefix("--runtime=") {
            runtime_kind = RuntimeKind::parse(value).map_err(StreamError::Configuration)?;
        } else if let Some(value) = a.strip_prefix("--output=") {
            output_png = PathBuf::from(value);
        }
    }

    println!("=== Polyglot Vulkan adapter ray-tracing scenario (#667) ===");
    println!("Runtime:     {}", runtime_kind.as_str());
    println!(
        "Surface:     {SURFACE_SIZE}x{SURFACE_SIZE} RGBA8 (uuid {SCENARIO_SURFACE_UUID})"
    );
    println!(
        "SPIR-V:      rgen={} bytes, rmiss={} bytes, rchit={} bytes",
        SCENE_RGEN_SPV.len(),
        SCENE_RMISS_SPV.len(),
        SCENE_RCHIT_SPV.len()
    );
    println!("Output PNG:  {}", output_png.display());
    println!();

    let runtime = StreamRuntime::new()?;

    let texture_slot: Arc<Mutex<Option<StreamTexture>>> = Arc::new(Mutex::new(None));
    let readback_slot: Arc<Mutex<Option<Arc<VulkanTextureReadback>>>> =
        Arc::new(Mutex::new(None));

    {
        let texture_slot = Arc::clone(&texture_slot);
        let readback_slot = Arc::clone(&readback_slot);
        runtime.install_setup_hook(move |gpu| {
            // Skip the whole setup if the device doesn't expose RT.
            // The bridge will be unset and the subprocess will receive
            // an "unsupported" error response, which the Python /
            // Deno entry points print and gracefully exit on.
            if !gpu.supports_ray_tracing_pipeline() {
                println!(
                    "✗ device does not expose VK_KHR_ray_tracing_pipeline. Skipping \
                     bridge setup; the polyglot processor will surface the \
                     unsupported error and exit."
                );
                return Ok(());
            }
            let host_device = Arc::clone(gpu.device().vulkan_device());
            let texture = HostVulkanTexture::new_device_local(
                &host_device,
                &TextureDescriptor {
                    label: Some("polyglot-rt/output"),
                    width: SURFACE_SIZE,
                    height: SURFACE_SIZE,
                    format: TextureFormat::Rgba8Unorm,
                    usage: TextureUsages::STORAGE_BINDING | TextureUsages::COPY_SRC,
                },
            )
            .map_err(|e| {
                StreamError::Configuration(format!(
                    "HostVulkanTexture::new_device_local: {e}"
                ))
            })?;
            let stream_texture = StreamTexture::from_vulkan(texture);
            // Storage-image binding requires `VK_IMAGE_LAYOUT_GENERAL`.
            let image = stream_texture
                .vulkan_inner()
                .image()
                .ok_or_else(|| {
                    StreamError::Configuration(
                        "freshly-created HostVulkanTexture missing VkImage handle".into(),
                    )
                })?;
            HostVulkanTexture::transition_to_general(&host_device, image).map_err(|e| {
                StreamError::Configuration(format!(
                    "transition output texture to GENERAL: {e}"
                ))
            })?;

            // Same-process registration so the bridge / readback can
            // resolve the texture by UUID. The scenario doesn't ride
            // the cross-process surface-share path — the bridge talks
            // directly to the in-tree handle map.
            gpu.register_texture_with_layout(
                SCENARIO_SURFACE_UUID,
                stream_texture.clone(),
                VulkanLayout::GENERAL,
            );

            let bridge = Arc::new(SceneKernelBridge::new(
                Arc::clone(&host_device),
                vec![(SCENARIO_SURFACE_UUID.to_string(), stream_texture.clone())],
            ));
            gpu.set_ray_tracing_kernel_bridge(bridge);

            let readback = gpu.create_texture_readback(&TextureReadbackDescriptor {
                label: "polyglot-vulkan-ray-tracing/readback",
                format: TextureFormat::Rgba8Unorm,
                width: SURFACE_SIZE,
                height: SURFACE_SIZE,
            })?;

            *texture_slot.lock().unwrap() = Some(stream_texture);
            *readback_slot.lock().unwrap() = Some(readback);
            println!(
                "✓ storage image registered as '{}'",
                SCENARIO_SURFACE_UUID
            );
            Ok(())
        });
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    match runtime_kind {
        RuntimeKind::Python => {
            let slpkg_path = manifest_dir
                .join("python/polyglot-vulkan-ray-tracing-0.1.0.slpkg");
            if !slpkg_path.exists() {
                return Err(StreamError::Configuration(format!(
                    "Package not found: {}\nRun: cargo run -p streamlib-cli -- pack examples/polyglot-vulkan-ray-tracing/python",
                    slpkg_path.display()
                )));
            }
            runtime.load_package(&slpkg_path)?;
        }
        RuntimeKind::Deno => {
            let project_path = manifest_dir.join("deno");
            if !project_path.join("streamlib.yaml").exists() {
                return Err(StreamError::Configuration(format!(
                    "Deno project not found: {}",
                    project_path.display()
                )));
            }
            runtime.load_project(&project_path)?;
        }
    }

    let fixture_path = write_trigger_fixture()
        .map_err(StreamError::Configuration)?;
    let source = runtime.add_processor(BgraFileSourceProcessor::Processor::node(
        BgraFileSourceProcessor::Config {
            file_path: fixture_path
                .to_str()
                .ok_or_else(|| {
                    StreamError::Configuration(
                        "fixture path has non-utf8 component".into(),
                    )
                })?
                .to_string(),
            width: 4,
            height: 4,
            fps: 5,
            frame_count: 3,
        },
    ))?;
    println!("+ BgraFileSource: {source}");

    let variant: u32 = match runtime_kind {
        RuntimeKind::Python => 0,
        RuntimeKind::Deno => 1,
    };
    let rt_config = serde_json::json!({
        "vulkan_surface_uuid": SCENARIO_SURFACE_UUID,
        "width": SURFACE_SIZE,
        "height": SURFACE_SIZE,
        "variant": variant,
        "rgen_spv_hex": bytes_to_hex(SCENE_RGEN_SPV),
        "rmiss_spv_hex": bytes_to_hex(SCENE_RMISS_SPV),
        "rchit_spv_hex": bytes_to_hex(SCENE_RCHIT_SPV),
    });
    let rt = runtime.add_processor(ProcessorSpec::new(
        runtime_kind.processor_name(),
        rt_config,
    ))?;
    println!("+ Vulkan ray-tracing processor: {rt}");

    runtime.connect(
        OutputLinkPortRef::new(&source, "video"),
        InputLinkPortRef::new(&rt, "video_in"),
    )?;
    println!(
        "\nPipeline: BgraFileSource → {} vulkan-ray-tracing\n",
        runtime_kind.as_str()
    );

    println!("Starting pipeline...");
    runtime.start()?;
    std::thread::sleep(Duration::from_secs(4));
    println!("Stopping pipeline...");
    runtime.stop()?;

    println!("\nReading host storage image back via Vulkan...");
    let texture = texture_slot
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| {
            StreamError::Runtime(
                "host texture slot is empty — setup hook never ran (likely \
                 because the device lacks RT support; see the earlier log)"
                    .into(),
            )
        })?;
    let readback = readback_slot
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| StreamError::Runtime("readback slot is empty".into()))?;
    let ticket = readback
        .submit(&texture, TextureSourceLayout::General)
        .map_err(|e| StreamError::Runtime(format!("readback submit: {e}")))?;
    let rgba = readback
        .wait_and_read(ticket, u64::MAX)
        .map_err(|e| StreamError::Runtime(format!("readback wait: {e}")))?
        .to_vec();
    write_png(&rgba, SURFACE_SIZE, SURFACE_SIZE, &output_png)?;
    println!("✓ Output PNG written: {}", output_png.display());

    Ok(())
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

fn write_trigger_fixture() -> std::result::Result<PathBuf, String> {
    use std::fs::File;
    use std::io::Write;

    let path = std::env::temp_dir().join("vulkan-rt-trigger.bgra");
    let mut f = File::create(&path)
        .map_err(|e| format!("create {}: {e}", path.display()))?;
    f.write_all(&[0u8; 4 * 4 * 4 * 3])
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}

fn write_png(
    rgba: &[u8],
    width: u32,
    height: u32,
    output: &std::path::Path,
) -> Result<()> {
    use std::fs::File;
    use std::io::BufWriter;

    let file = File::create(output).map_err(|e| {
        StreamError::Configuration(format!("create output PNG {}: {e}", output.display()))
    })?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| StreamError::Configuration(format!("PNG header: {e}")))?;
    writer
        .write_image_data(rgba)
        .map_err(|e| StreamError::Configuration(format!("PNG body: {e}")))?;
    Ok(())
}

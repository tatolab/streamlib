// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
#[cfg(target_os = "linux")]
use crate::vulkan::rhi::memory_and_buffers::vulkan_frame_allocation_device_memory_budget::DeviceMemoryHeapBudgetReading;

/// The device this milestone exists to bring up. Unlike [`try_create_device`]
/// this refuses to pass by skipping: under `hardware-tests` the rig is
/// asserted to exist, and a MoltenVK that cannot produce a device is the
/// failure the test is for.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_device_comes_up_on_this_hosts_driver_and_names_itself() {
    let device = HostVulkanDevice::new()
        .expect("the rig must produce a Vulkan device — on Apple that is MoltenVK");

    let device_name = device.name();
    assert!(
        !device_name.trim().is_empty(),
        "a device that cannot name itself is not a device the engine can report on"
    );
    println!("Vulkan device: {device_name}");
}

/// Ray tracing is an absent tier on MoltenVK, not a failure: construction
/// refuses by name rather than reaching an entry point that never loaded.
#[cfg(target_os = "macos")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn ray_tracing_on_a_device_without_it_refuses_rather_than_panicking() {
    let device = HostVulkanDevice::new().expect("the rig must produce a Vulkan device");
    if device.supports_ray_tracing_pipeline() {
        println!("skipping — this device serves ray tracing, so there is no tier to refuse");
        return;
    }

    let unit_triangle_vertices = [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
    let unit_triangle_indices = [0u32, 1, 2];
    let refusal = crate::vulkan::rhi::VulkanAccelerationStructure::build_triangles_blas(
        &device,
        "absent-tier/blas",
        &unit_triangle_vertices,
        &unit_triangle_indices,
    )
    .expect_err("a device without ray tracing must refuse to build an acceleration structure");
    assert!(
        matches!(refusal, Error::GpuError(_)),
        "the refusal must be the engine's typed GPU error, got {refusal:?}"
    );
}

/// Past a budget with no room left, a frame texture is refused by name
/// before anything is allocated, while a uniform buffer — engine memory
/// the budget never weighs — still allocates.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
#[allow(clippy::disallowed_macros)]
fn a_frame_texture_past_a_budget_with_no_room_left_is_refused_while_an_engine_buffer_still_allocates()
 {
    use crate::core::rhi::{TextureFormat, TextureUsages};

    let Some(device) = try_create_device() else {
        return;
    };
    let device_memory_blocks_the_allocator_holds = || -> u64 {
        device
            .allocator()
            .get_heap_budgets()
            .expect("the allocator reports every heap")
            .iter()
            .map(|heap_budget| u64::from(heap_budget.statistics.blockCount))
            .sum()
    };
    let frame_descriptor = TextureDescriptor::new(256, 256, TextureFormat::Bgra8Unorm).with_usage(
        TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_SRC | TextureUsages::COPY_DST,
    );

    let _frame_allocation_budget_left_with_no_room =
        device.leave_no_room_in_any_frame_allocation_budget_for_this_test();
    let blocks_before_the_refusal = device_memory_blocks_the_allocator_holds();
    let refusal = device
        .create_texture(&frame_descriptor)
        .err()
        .expect("a frame past a budget with no room left is refused");
    println!("refused: {refusal}");

    #[cfg(target_os = "linux")]
    match &refusal {
        Error::FrameAllocationRefusedPastTheDeviceMemoryHeapBudget {
            heap_index,
            frame_allocation_site,
            ..
        } => {
            assert_eq!(frame_allocation_site, "HostVulkanTexture::new");
            if let Some(dma_buf_image_pool) = device.dma_buf_image_pool_pinned_to_one_memory_type()
            {
                assert_eq!(
                    *heap_index,
                    device
                        .device_memory_heap_of_memory_type(dma_buf_image_pool.memory_type_index)
                        .expect("the pool's pinned type is one of the device's"),
                    "the refusal names the heap the DMA-BUF image pool's pinned memory type \
                     allocates from: {refusal}"
                );
            }
        }
        other => panic!("expected the heap-budget refusal, got {other:?}"),
    }
    #[cfg(target_os = "macos")]
    assert!(
        matches!(
            refusal,
            Error::FrameAllocationRefusedPastTheMetalWorkingSetBudget { .. }
        ),
        "expected the Metal working-set refusal, got {refusal:?}"
    );
    assert_eq!(
        device_memory_blocks_the_allocator_holds(),
        blocks_before_the_refusal,
        "a refused frame allocates no device memory"
    );

    crate::vulkan::rhi::HostVulkanBuffer::new_uniform_buffer_host_visible(&device, 256)
        .expect("a uniform buffer the budget never weighs still allocates past it");
}

/// Every VMA frame constructor, past a budget with no room left, is
/// refused naming the heap that the same constructor's allocation lands on.
#[cfg(target_os = "linux")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
#[allow(clippy::disallowed_macros)]
fn every_vma_frame_constructors_refusal_names_the_heap_its_allocation_lands_on() {
    use crate::core::rhi::{TextureFormat, TextureUsages};
    use crate::vulkan::rhi::HostVulkanBuffer;
    use crate::vulkan::rhi::device::drm_modifier_probe::fourcc;

    type MemoryTypeOfOneFreshFrameAllocation<'a> = Box<dyn Fn() -> Result<u32> + 'a>;
    fn memory_type_of_the_texture(texture: HostVulkanTexture) -> u32 {
        texture
            .vma_allocation_memory_type_index()
            .expect("a VMA-allocated texture has a memory type")
    }
    fn memory_type_of_the_buffer(buffer: HostVulkanBuffer) -> u32 {
        buffer
            .vma_allocation_memory_type_index()
            .expect("a VMA-allocated buffer has a memory type")
    }

    let Some(device) = try_create_device() else {
        return;
    };
    const FRAME_EDGE_PIXELS: u32 = 256;
    let frame_byte_len = u64::from(FRAME_EDGE_PIXELS * FRAME_EDGE_PIXELS * 4);
    let sampled_frame_descriptor = TextureDescriptor::new(
        FRAME_EDGE_PIXELS,
        FRAME_EDGE_PIXELS,
        TextureFormat::Bgra8Unorm,
    )
    .with_usage(TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_SRC | TextureUsages::COPY_DST);
    let render_target_frame_descriptor = TextureDescriptor::new(
        FRAME_EDGE_PIXELS,
        FRAME_EDGE_PIXELS,
        TextureFormat::Bgra8Unorm,
    )
    .with_usage(
        TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_SRC,
    );
    let cuda_mappable_frame_descriptor = TextureDescriptor::new(
        FRAME_EDGE_PIXELS,
        FRAME_EDGE_PIXELS,
        TextureFormat::Rgba8Unorm,
    );
    let render_target_modifiers = device
        .drm_modifier_table()
        .rt_modifiers(fourcc::DRM_FORMAT_ARGB8888);
    let host_cached_opaque_fd_buffer_site = if device
        .opaque_fd_buffer_pool_host_cached_pinned_to_one_memory_type()
        .is_some()
    {
        "HostVulkanBuffer::new_opaque_fd_export_host_cached"
    } else {
        "HostVulkanBuffer::new_opaque_fd_export"
    };
    const DMA_BUF_RENDER_TARGET_FRAME_SITE: &str = "HostVulkanTexture::new_render_target_dma_buf";
    let frame_constructors_by_site: Vec<(&str, MemoryTypeOfOneFreshFrameAllocation<'_>)> = vec![
        (
            "HostVulkanTexture::new",
            Box::new(|| {
                HostVulkanTexture::new(&device, &sampled_frame_descriptor)
                    .map(memory_type_of_the_texture)
            }),
        ),
        (
            "HostVulkanTexture::new_device_local",
            Box::new(|| {
                HostVulkanTexture::new_device_local(&device, &sampled_frame_descriptor)
                    .map(memory_type_of_the_texture)
            }),
        ),
        (
            DMA_BUF_RENDER_TARGET_FRAME_SITE,
            Box::new(|| {
                HostVulkanTexture::new_render_target_dma_buf(
                    &device,
                    &render_target_frame_descriptor,
                    render_target_modifiers,
                )
                .map(memory_type_of_the_texture)
            }),
        ),
        (
            "HostVulkanTexture::new_opaque_fd_export",
            Box::new(|| {
                HostVulkanTexture::new_opaque_fd_export(&device, &cuda_mappable_frame_descriptor)
                    .map(memory_type_of_the_texture)
            }),
        ),
        (
            "HostVulkanBuffer::new",
            Box::new(|| {
                HostVulkanBuffer::new(&device, frame_byte_len).map(memory_type_of_the_buffer)
            }),
        ),
        (
            "HostVulkanBuffer::new_storage_buffer_host_cached_for_cpu_reads",
            Box::new(|| {
                HostVulkanBuffer::new_storage_buffer_host_cached_for_cpu_reads(
                    &device,
                    frame_byte_len,
                )
                .map(memory_type_of_the_buffer)
            }),
        ),
        (
            "HostVulkanBuffer::new_opaque_fd_export",
            Box::new(|| {
                HostVulkanBuffer::new_opaque_fd_export(&device, frame_byte_len)
                    .map(memory_type_of_the_buffer)
            }),
        ),
        (
            host_cached_opaque_fd_buffer_site,
            Box::new(|| {
                HostVulkanBuffer::new_opaque_fd_export_host_cached(&device, frame_byte_len)
                    .map(memory_type_of_the_buffer)
            }),
        ),
        (
            "HostVulkanBuffer::new_opaque_fd_export_device_local",
            Box::new(|| {
                HostVulkanBuffer::new_opaque_fd_export_device_local(&device, frame_byte_len)
                    .map(memory_type_of_the_buffer)
            }),
        ),
    ];

    let heap_each_frame_constructors_allocation_lands_on: Vec<_> = frame_constructors_by_site
        .iter()
        .filter_map(
            |(frame_allocation_site, allocate_one_frame)| match allocate_one_frame() {
                Ok(memory_type_index) => Some((
                    *frame_allocation_site,
                    allocate_one_frame,
                    device
                        .device_memory_heap_of_memory_type(memory_type_index)
                        .expect("the allocation's memory type is one of the device's"),
                )),
                Err(e)
                    if *frame_allocation_site == DMA_BUF_RENDER_TARGET_FRAME_SITE
                        && !device.supports_dma_buf_export() =>
                {
                    skip_because_this_machine_lacks_naming_the_refusal(
                        RigOnlyCapability::DmaBufModifiers,
                        format!("{frame_allocation_site} does not allocate here: {e}"),
                    );
                    None
                }
                Err(e) => {
                    println!("skipping {frame_allocation_site}: it does not allocate here: {e}");
                    None
                }
            },
        )
        .collect();
    assert!(
        !heap_each_frame_constructors_allocation_lands_on.is_empty(),
        "a device that allocates no frame through any constructor has nothing to weigh"
    );

    let _frame_allocation_budget_left_with_no_room =
        device.leave_no_room_in_any_frame_allocation_budget_for_this_test();
    for (expected_frame_allocation_site, allocate_one_frame, heap_the_allocation_lands_on) in
        heap_each_frame_constructors_allocation_lands_on
    {
        match allocate_one_frame() {
            Err(Error::FrameAllocationRefusedPastTheDeviceMemoryHeapBudget {
                heap_index,
                frame_allocation_site,
                ..
            }) => {
                assert_eq!(frame_allocation_site, expected_frame_allocation_site);
                assert_eq!(
                    heap_index, heap_the_allocation_lands_on,
                    "{expected_frame_allocation_site} is refused naming heap {heap_index}, \
                     but its allocation lands on heap {heap_the_allocation_lands_on}"
                );
                println!(
                    "{expected_frame_allocation_site}: refused naming heap {heap_index}, the \
                     heap its allocation lands on"
                );
            }
            Ok(_) => {
                panic!("{expected_frame_allocation_site} allocated past a budget with no room left")
            }
            Err(other) => panic!(
                "{expected_frame_allocation_site}: expected the heap-budget refusal, got \
                 {other:?}"
            ),
        }
    }
}

/// The rig's evidence that GPU memory another process holds reaches this
/// runtime as a smaller heap budget: a child holds a CUDA allocation and
/// the largest device-local heap's driver-reported budget falls by it.
#[cfg(target_os = "linux")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
#[allow(clippy::disallowed_macros)]
fn the_heap_budget_the_device_reads_falls_while_another_process_holds_a_large_cuda_allocation() {
    use std::io::{BufRead as _, BufReader};
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::Duration;

    use crate::core::error::DeviceMemoryHeapLocality;

    const PYTHON_WITH_CUDA_TORCH_VARIABLE: &str = "STREAMLIB_RIG_PYTHON_WITH_CUDA_TORCH";
    const LARGEST_CHILD_CUDA_ALLOCATION_BYTES: u64 = 4 << 30;
    const SMALLEST_CHILD_CUDA_ALLOCATION_WORTH_MEASURING_BYTES: u64 = 256 << 20;
    const CHILD_HOLDS_ITS_ALLOCATION_WITHIN: Duration = Duration::from_secs(120);

    let Ok(python_with_cuda_torch) = std::env::var(PYTHON_WITH_CUDA_TORCH_VARIABLE) else {
        skip_because_this_machine_lacks(RigOnlyCapability::Cuda);
        return;
    };
    let Some(device) = try_create_device() else {
        return;
    };
    if !device.has_memory_budget {
        println!(
            "Skipping - VK_EXT_memory_budget is not advertised, so no driver-reported \
             budget exists to fall"
        );
        return;
    }
    let largest_device_local_heap = |every_heaps_reading: &[DeviceMemoryHeapBudgetReading]| {
        every_heaps_reading
            .iter()
            .filter(|heap| heap.heap_locality == DeviceMemoryHeapLocality::DeviceLocal)
            .max_by_key(|heap| heap.heap_budget_bytes)
            .cloned()
            .expect("a GPU has a device-local heap")
    };

    let heap_before = largest_device_local_heap(
        &device
            .read_every_heaps_device_memory_budget_fetched_from_the_driver_now()
            .expect("every heap's budget reads before the child allocates"),
    );
    let heap_room_before = heap_before
        .heap_budget_bytes
        .saturating_sub(heap_before.heap_usage_bytes);
    let child_cuda_allocation_bytes =
        (heap_room_before / 4).min(LARGEST_CHILD_CUDA_ALLOCATION_BYTES);
    if child_cuda_allocation_bytes < SMALLEST_CHILD_CUDA_ALLOCATION_WORTH_MEASURING_BYTES {
        println!(
            "Skipping - heap {} has {heap_room_before} bytes of room, too little to measure a \
             child's allocation against",
            heap_before.heap_index
        );
        return;
    }

    let child_script = format!(
        "import sys, torch\n\
         print('torch', torch.__version__, 'cuda', torch.cuda.is_available(), flush=True)\n\
         if not torch.cuda.is_available():\n    sys.exit(3)\n\
         held = torch.empty({child_cuda_allocation_bytes}, dtype=torch.uint8, device='cuda')\n\
         torch.cuda.synchronize()\n\
         print('held', flush=True)\n\
         sys.stdin.read()\n"
    );
    let mut child = Command::new(&python_with_cuda_torch)
        .arg("-c")
        .arg(&child_script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("the python named by the variable starts");
    let child_stdout = child.stdout.take().expect("the child's stdout is piped");
    let (child_line_tx, child_line_rx) = mpsc::channel::<String>();
    let child_stdout_reader = std::thread::spawn(move || {
        for line in BufReader::new(child_stdout)
            .lines()
            .map_while(|line| line.ok())
        {
            if child_line_tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut child_holds_its_cuda_allocation = false;
    while let Ok(line) = child_line_rx.recv_timeout(CHILD_HOLDS_ITS_ALLOCATION_WITHIN) {
        println!("child: {line}");
        if line == "held" {
            child_holds_its_cuda_allocation = true;
            break;
        }
    }
    if !child_holds_its_cuda_allocation {
        let _ = child.kill();
        let child_status = child.wait();
        let _ = child_stdout_reader.join();
        println!(
            "Skipping - the child exited or stalled before holding a \
             {child_cuda_allocation_bytes}-byte CUDA allocation ({child_status:?}); a +cpu \
             torch exits 3"
        );
        return;
    }

    let every_heaps_reading_while_the_child_holds = device
        .read_every_heaps_device_memory_budget_fetched_from_the_driver_now()
        .expect("every heap's budget reads while the child holds its allocation");
    drop(child.stdin.take());
    let child_status = child.wait().expect("the child exits once its stdin closes");
    let _ = child_stdout_reader.join();
    let heap_while_the_child_holds = every_heaps_reading_while_the_child_holds
        .into_iter()
        .nth(heap_before.heap_index as usize)
        .expect("the same heap reads again");

    println!("child CUDA allocation: {child_cuda_allocation_bytes} bytes ({child_status})");
    println!("heap before the child allocated: {heap_before:?}");
    println!("heap while the child held it:    {heap_while_the_child_holds:?}");
    let heap_budget_fall = heap_before
        .heap_budget_bytes
        .saturating_sub(heap_while_the_child_holds.heap_budget_bytes);
    assert!(
        heap_budget_fall >= child_cuda_allocation_bytes / 4 * 3,
        "heap {}'s budget fell by {heap_budget_fall} bytes while another process held \
         {child_cuda_allocation_bytes} bytes of CUDA memory",
        heap_before.heap_index
    );
}

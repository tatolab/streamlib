// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of VulkanCommandBufferPool.h + VulkanCommandBufferPool.cpp
//!
//! Manages a pool of Vulkan command buffers for decode/encode operations.
//! Each pool node tracks command buffer state (reset/recording/recorded/submitted),
//! fence, semaphore, and query pool associations.
//!
//! Key divergences from C++:
//! - The C++ uses `VkSharedBaseObj` (shared_ptr with custom deleter) for RAII pool node
//!   checkout. We use `Arc<Mutex<...>>` for the pool and return a `PoolNodeHandle` that
//!   releases back to the pool on `Drop`.
//! - The C++ `PoolNodeHandler` RAII wrapper is ported as `PoolNodeHandler` with a manual
//!   `finish()` method instead of relying on destructor side effects for submission.
//! - Sub-resources (command buffer set, fence set, semaphore set, query pool set) are
//!   inlined as fields rather than separate types, matching the C++ architecture but
//!   using idiomatic Rust vectors.
//! - `VulkanDeviceContext` is replaced by `vulkanalia::Device` plus the raw `vk::Device` handle.

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use std::cell::UnsafeCell;
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum number of pool nodes (matches C++ `maxPoolNodes = 64`).
pub const MAX_POOL_NODES: usize = 64;

/// Default per-iteration fence wait timeout: 100 ms in nanoseconds.
const DEFAULT_FENCE_WAIT_TIMEOUT_NSEC: u64 = 100 * 1000 * 1000;

/// Default total fence wait timeout: 5 seconds in nanoseconds.
const DEFAULT_FENCE_TOTAL_WAIT_TIMEOUT_NSEC: u64 = 5 * 1000 * 1000 * 1000;

// ---------------------------------------------------------------------------
// CmdBufState
// ---------------------------------------------------------------------------

/// Command buffer lifecycle states, mirroring C++ `PoolNode::CmdBufState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmdBufState {
    Reset = 0,
    Recording,
    Recorded,
    Submitted,
}

// ---------------------------------------------------------------------------
// PoolNode
// ---------------------------------------------------------------------------

/// Per-slot state for a command buffer within the pool.
///
/// Mirrors C++ `VulkanCommandBufferPool::PoolNode`.
///
/// In the C++ code, PoolNode holds a back-pointer to its parent pool and an
/// index. Here we store the index and provide access to pool resources via the
/// `VulkanCommandBufferPool` methods, passing the device and pool reference
/// explicitly where needed.
pub struct PoolNode {
    device: Option<vulkanalia::Device>,
    parent_index: i32,
    cmd_buf_state: CmdBufState,
}

impl PoolNode {
    /// Create a new uninitialized pool node.
    pub fn new() -> Self {
        Self {
            device: None,
            parent_index: -1,
            cmd_buf_state: CmdBufState::Reset,
        }
    }

    /// Initialize the node with a device reference.
    /// Mirrors C++ `PoolNode::Init`.
    pub fn init(&mut self, device: vulkanalia::Device) -> vk::Result {
        self.device = Some(device);
        vk::Result::SUCCESS
    }

    /// Set the parent pool index.
    /// Mirrors C++ `PoolNode::SetParent`.
    pub fn set_parent(&mut self, parent_index: i32) -> vk::Result {
        debug_assert_eq!(self.parent_index, -1, "PoolNode already has a parent");
        self.parent_index = parent_index;
        vk::Result::SUCCESS
    }

    /// Clear the parent association.
    /// Mirrors C++ `PoolNode::ClearParent`.
    ///
    /// NOTE: Does NOT reset `cmd_buf_state`. The state must persist across
    /// pool release/reacquire so that `reset_command_buffer()` can properly
    /// wait on and reset the fence when the node is reused. (See C++ comment.)
    pub fn clear_parent(&mut self) {
        self.parent_index = -1;
    }

    /// Deinitialize the node.
    /// Mirrors C++ `PoolNode::Deinit`.
    pub fn deinit(&mut self) {
        self.clear_parent();
        self.device = None;
    }

    /// Return the current parent index, or `None` if not assigned.
    pub fn parent_index(&self) -> Option<u32> {
        if self.parent_index < 0 {
            None
        } else {
            Some(self.parent_index as u32)
        }
    }

    /// Get the current command buffer state.
    pub fn cmd_buf_state(&self) -> CmdBufState {
        self.cmd_buf_state
    }

    /// Get the pool node index.
    /// Mirrors C++ `PoolNode::GetNodePoolIndex`.
    pub fn get_node_pool_index(&self) -> Option<u32> {
        self.parent_index()
    }

    /// Check that the node is in a valid state (has device and parent index).
    fn is_valid(&self) -> bool {
        self.device.is_some() && self.parent_index >= 0
    }

    /// Begin command buffer recording.
    /// Mirrors C++ `PoolNode::BeginCommandBufferRecording`.
    ///
    /// # Safety
    /// The caller must ensure the command buffer handle is valid and the device
    /// is not lost.
    pub unsafe fn begin_command_buffer_recording(
        &mut self,
        cmd_buf: vk::CommandBuffer,
        begin_info: &vk::CommandBufferBeginInfo,
    ) -> Result<vk::CommandBuffer, vk::Result> {
        if !self.is_valid() {
            return Err(vk::Result::ERROR_INITIALIZATION_FAILED);
        }
        if self.cmd_buf_state != CmdBufState::Reset {
            return Err(vk::Result::ERROR_INITIALIZATION_FAILED);
        }

        let device = self.device.as_ref().unwrap();
        device.begin_command_buffer(cmd_buf, begin_info)?;
        self.cmd_buf_state = CmdBufState::Recording;
        Ok(cmd_buf)
    }

    /// End command buffer recording.
    /// Mirrors C++ `PoolNode::EndCommandBufferRecording`.
    ///
    /// # Safety
    /// The caller must ensure the command buffer handle is valid.
    pub unsafe fn end_command_buffer_recording(
        &mut self,
        cmd_buf: vk::CommandBuffer,
    ) -> vk::Result {
        if !self.is_valid() {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        }
        if self.cmd_buf_state != CmdBufState::Recording {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        }

        let device = self.device.as_ref().unwrap();
        match device.end_command_buffer(cmd_buf) {
            Ok(()) => {
                self.cmd_buf_state = CmdBufState::Recorded;
                vk::Result::SUCCESS
            }
            Err(e) => vk::Result::from(e),
        }
    }

    /// Mark the command buffer as submitted.
    /// Mirrors C++ `PoolNode::SetCommandBufferSubmitted`.
    pub fn set_command_buffer_submitted(&mut self) -> bool {
        if !self.is_valid() {
            return false;
        }
        if self.cmd_buf_state != CmdBufState::Recorded {
            return false;
        }
        self.cmd_buf_state = CmdBufState::Submitted;
        true
    }

    /// Wait on the host for command buffer completion using a fence, then
    /// optionally reset the fence.
    /// Mirrors C++ `PoolNode::SyncHostOnCmdBuffComplete`.
    ///
    /// # Safety
    /// The fence must be valid and associated with a submitted command buffer.
    pub unsafe fn sync_host_on_cmd_buff_complete(
        &self,
        fence: vk::Fence,
        reset_after_wait: bool,
        fence_name: &str,
        fence_wait_timeout_nsec: u64,
        fence_total_wait_timeout_nsec: u64,
    ) -> vk::Result {
        if self.cmd_buf_state != CmdBufState::Submitted {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        }

        let device = match &self.device {
            Some(d) => d,
            None => return vk::Result::ERROR_INITIALIZATION_FAILED,
        };
        if fence == vk::Fence::null() {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        }

        let result = wait_and_reset_fence(
            device,
            fence,
            reset_after_wait,
            fence_name,
            fence_wait_timeout_nsec,
            fence_total_wait_timeout_nsec,
        );

        if result != vk::Result::SUCCESS {
            eprintln!(
                "\nERROR: wait_and_reset_fence() for {} with result: {:?}",
                fence_name, result
            );
        }

        result
    }

    /// Reset the command buffer state, optionally waiting for GPU completion.
    /// Mirrors C++ `PoolNode::ResetCommandBuffer`.
    ///
    /// # Safety
    /// If `sync_with_host` is true, the fence must be valid.
    pub unsafe fn reset_command_buffer(
        &mut self,
        fence: vk::Fence,
        sync_with_host: bool,
        fence_name: &str,
    ) -> bool {
        if self.cmd_buf_state == CmdBufState::Reset {
            return false;
        }

        if sync_with_host {
            self.sync_host_on_cmd_buff_complete(
                fence,
                true,
                fence_name,
                DEFAULT_FENCE_WAIT_TIMEOUT_NSEC,
                DEFAULT_FENCE_TOTAL_WAIT_TIMEOUT_NSEC,
            );
        }

        self.cmd_buf_state = CmdBufState::Reset;
        true
    }
}

impl Default for PoolNode {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// VulkanCommandBuffersSet (inline helper)
// ---------------------------------------------------------------------------

/// Manages a `VkCommandPool` and its allocated command buffers.
/// Mirrors C++ `VulkanCommandBuffersSet`.
struct VulkanCommandBuffersSet {
    device: Option<vulkanalia::Device>,
    cmd_pool: vk::CommandPool,
    cmd_buffers: Vec<vk::CommandBuffer>,
}

impl VulkanCommandBuffersSet {
    fn new() -> Self {
        Self {
            device: None,
            cmd_pool: vk::CommandPool::null(),
            cmd_buffers: Vec::new(),
        }
    }

    /// Create a command pool and allocate command buffers.
    ///
    /// # Safety
    /// `device` must be valid and not destroyed for the lifetime of this set.
    unsafe fn create_command_buffer_pool(
        &mut self,
        device: &vulkanalia::Device,
        queue_family_index: u32,
        count: u32,
    ) -> vk::Result {
        let pool_info = vk::CommandPoolCreateInfo::builder()
            .queue_family_index(queue_family_index)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);

        self.cmd_pool = match device.create_command_pool(&pool_info, None) {
            Ok(pool) => pool,
            Err(e) => return e.into(),
        };

        let alloc_info = vk::CommandBufferAllocateInfo::builder()
            .command_pool(self.cmd_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(count);

        self.cmd_buffers = match device.allocate_command_buffers(&alloc_info) {
            Ok(bufs) => bufs,
            Err(e) => {
                device.destroy_command_pool(self.cmd_pool, None);
                self.cmd_pool = vk::CommandPool::null();
                return e.into();
            }
        };

        self.device = Some(device.clone());
        vk::Result::SUCCESS
    }

    fn get_command_buffer(&self, index: u32) -> Option<vk::CommandBuffer> {
        self.cmd_buffers.get(index as usize).copied()
    }

    /// # Safety
    /// Must only be called when no command buffers are in use.
    unsafe fn destroy(&mut self) {
        if let Some(ref device) = self.device {
            if !self.cmd_buffers.is_empty() {
                device.free_command_buffers(self.cmd_pool, &self.cmd_buffers);
                self.cmd_buffers.clear();
            }
            if self.cmd_pool != vk::CommandPool::null() {
                device.destroy_command_pool(self.cmd_pool, None);
                self.cmd_pool = vk::CommandPool::null();
            }
        }
        self.device = None;
    }
}

// ---------------------------------------------------------------------------
// VulkanFenceSet (inline helper)
// ---------------------------------------------------------------------------

/// Manages a set of `VkFence` objects.
/// Mirrors C++ `VulkanFenceSet`.
struct VulkanFenceSet {
    device: Option<vulkanalia::Device>,
    fences: Vec<vk::Fence>,
}

impl VulkanFenceSet {
    fn new() -> Self {
        Self {
            device: None,
            fences: Vec::new(),
        }
    }

    /// # Safety
    /// `device` must be valid and not destroyed for the lifetime of this set.
    unsafe fn create_set(&mut self, device: &vulkanalia::Device, count: u32) -> vk::Result {
        self.destroy();

        let fence_info = vk::FenceCreateInfo::default();
        let mut fences = Vec::with_capacity(count as usize);
        for _ in 0..count {
            match device.create_fence(&fence_info, None) {
                Ok(f) => fences.push(f),
                Err(e) => {
                    // Clean up already-created fences.
                    for f in &fences {
                        device.destroy_fence(*f, None);
                    }
                    return e.into();
                }
            }
        }
        self.fences = fences;
        self.device = Some(device.clone());
        vk::Result::SUCCESS
    }

    fn get_fence(&self, index: u32) -> vk::Fence {
        self.fences
            .get(index as usize)
            .copied()
            .unwrap_or(vk::Fence::null())
    }

    /// # Safety
    /// No fences may be in use on the GPU.
    unsafe fn destroy(&mut self) {
        if let Some(ref device) = self.device {
            for f in &self.fences {
                if *f != vk::Fence::null() {
                    device.destroy_fence(*f, None);
                }
            }
        }
        self.fences.clear();
        self.device = None;
    }
}

// ---------------------------------------------------------------------------
// VulkanSemaphoreSet (inline helper)
// ---------------------------------------------------------------------------

/// Manages a set of `VkSemaphore` objects.
/// Mirrors C++ `VulkanSemaphoreSet`.
struct VulkanSemaphoreSet {
    device: Option<vulkanalia::Device>,
    semaphores: Vec<vk::Semaphore>,
}

impl VulkanSemaphoreSet {
    fn new() -> Self {
        Self {
            device: None,
            semaphores: Vec::new(),
        }
    }

    /// # Safety
    /// `device` must be valid and not destroyed for the lifetime of this set.
    unsafe fn create_set(&mut self, device: &vulkanalia::Device, count: u32) -> vk::Result {
        self.destroy();

        let sem_info = vk::SemaphoreCreateInfo::default();
        let mut sems = Vec::with_capacity(count as usize);
        for _ in 0..count {
            match device.create_semaphore(&sem_info, None) {
                Ok(s) => sems.push(s),
                Err(e) => {
                    for s in &sems {
                        device.destroy_semaphore(*s, None);
                    }
                    return e.into();
                }
            }
        }
        self.semaphores = sems;
        self.device = Some(device.clone());
        vk::Result::SUCCESS
    }

    fn get_semaphore(&self, index: u32) -> vk::Semaphore {
        self.semaphores
            .get(index as usize)
            .copied()
            .unwrap_or(vk::Semaphore::null())
    }

    /// # Safety
    /// No semaphores may be in use on the GPU.
    unsafe fn destroy(&mut self) {
        if let Some(ref device) = self.device {
            for s in &self.semaphores {
                if *s != vk::Semaphore::null() {
                    device.destroy_semaphore(*s, None);
                }
            }
        }
        self.semaphores.clear();
        self.device = None;
    }
}

// ---------------------------------------------------------------------------
// VulkanQueryPoolSet (inline helper)
// ---------------------------------------------------------------------------

/// Manages a single `VkQueryPool`.
/// Mirrors C++ `VulkanQueryPoolSet`.
struct VulkanQueryPoolSet {
    device: Option<vulkanalia::Device>,
    query_pool: vk::QueryPool,
    query_count: u32,
}

impl VulkanQueryPoolSet {
    fn new() -> Self {
        Self {
            device: None,
            query_pool: vk::QueryPool::null(),
            query_count: 0,
        }
    }

    /// # Safety
    /// `device` must be valid and not destroyed for the lifetime of this set.
    unsafe fn create_set(
        &mut self,
        device: &vulkanalia::Device,
        count: u32,
        query_type: vk::QueryType,
        flags: vk::QueryPoolCreateFlags,
        next: *const std::ffi::c_void,
    ) -> vk::Result {
        self.destroy();

        let _ = flags; // C++ passes flags as VkQueryPoolCreateFlags (reserved, must be 0).
        let mut create_info = vk::QueryPoolCreateInfo::builder()
            .query_type(query_type)
            .query_count(count);

        if !next.is_null() {
            create_info.next = next;
        }

        match device.create_query_pool(&create_info, None) {
            Ok(pool) => {
                self.query_pool = pool;
                self.query_count = count;
                self.device = Some(device.clone());
                vk::Result::SUCCESS
            }
            Err(e) => e.into(),
        }
    }

    fn get_query_pool(&self, query_idx: u32) -> vk::QueryPool {
        if query_idx < self.query_count {
            self.query_pool
        } else {
            vk::QueryPool::null()
        }
    }

    /// # Safety
    /// The query pool must not be in use on the GPU.
    unsafe fn destroy(&mut self) {
        if let Some(ref device) = self.device {
            if self.query_pool != vk::QueryPool::null() {
                device.destroy_query_pool(self.query_pool, None);
                self.query_pool = vk::QueryPool::null();
                self.query_count = 0;
            }
        }
        self.device = None;
    }
}

// ---------------------------------------------------------------------------
// wait_and_reset_fence (free function)
// ---------------------------------------------------------------------------

/// Wait for a fence to be signaled, with a polling loop and total timeout.
/// Optionally resets the fence after a successful wait.
/// Mirrors C++ `vk::WaitAndResetFence` from Helpers.h.
///
/// # Safety
/// The fence must be valid and the device must not be lost.
pub unsafe fn wait_and_reset_fence(
    device: &vulkanalia::Device,
    fence: vk::Fence,
    reset_after_wait: bool,
    fence_name: &str,
    fence_wait_timeout: u64,
    fence_total_wait_timeout: u64,
) -> vk::Result {
    debug_assert!(fence != vk::Fence::null());

    let mut current_wait: u64 = 0;
    let mut result = vk::Result::SUCCESS;

    while fence_total_wait_timeout >= current_wait {
        current_wait += fence_wait_timeout;

        match device.wait_for_fences(&[fence], true, fence_wait_timeout) {
            Ok(vk::SuccessCode::SUCCESS) => {
                result = vk::Result::SUCCESS;
                break;
            }
            Ok(vk::SuccessCode::TIMEOUT) => {
                eprintln!(
                    "\t **** WARNING: fence {}({:?}) is not done after {} mSec ****",
                    fence_name,
                    fence,
                    current_wait / (1000 * 1000)
                );
                result = vk::Result::TIMEOUT;
            }
            Ok(_) => {
                result = vk::Result::SUCCESS;
                break;
            }
            Err(e) => {
                result = e.into();
                break;
            }
        }
    }

    if result != vk::Result::SUCCESS {
        eprintln!(
            "\t **** ERROR: fence {}({:?}) is not done after {} mSec with status {:?} ****",
            fence_name,
            fence,
            fence_total_wait_timeout / (1000 * 1000),
            device.get_fence_status(fence),
        );
        return result;
    }

    if reset_after_wait {
        if let Err(e) = device.reset_fences(&[fence]) {
            return e.into();
        }
    }

    vk::Result::SUCCESS
}

// ---------------------------------------------------------------------------
// VulkanCommandBufferPool
// ---------------------------------------------------------------------------

/// A pool of Vulkan command buffers with associated fences, semaphores, and
/// query pools.
///
/// Mirrors C++ `VulkanCommandBufferPool`.
///
/// Thread safety: the inner state is behind a `Mutex`. The pool is wrapped in
/// `Arc` so that pool-node handles can release back to the pool when dropped.
pub struct VulkanCommandBufferPool {
    device: Option<vulkanalia::Device>,
    mutex: Mutex<PoolInner>,
    command_buffers_set: VulkanCommandBuffersSet,
    semaphore_set: VulkanSemaphoreSet,
    fence_set: VulkanFenceSet,
    query_pool_set: VulkanQueryPoolSet,
    pool_nodes: UnsafeCell<Vec<PoolNode>>,
}

// SAFETY: Access to pool_nodes is guarded by the bitmask in PoolInner (behind Mutex).
// Each node index is exclusively owned by at most one PoolNodeHandle at a time.
unsafe impl Send for VulkanCommandBufferPool {}
unsafe impl Sync for VulkanCommandBufferPool {}

/// Mutable state protected by the pool's mutex.
struct PoolInner {
    pool_size: u32,
    next_node_to_use: u32,
    available_pool_nodes: u64,
    queue_family_index: u32,
}

impl PoolInner {
    fn new() -> Self {
        Self {
            pool_size: 0,
            next_node_to_use: 0,
            available_pool_nodes: 0,
            queue_family_index: u32::MAX,
        }
    }
}

impl VulkanCommandBufferPool {
    /// Create a new, unconfigured pool.
    /// Mirrors C++ `VulkanCommandBufferPool::Create`.
    pub fn new(device: vulkanalia::Device) -> Arc<Self> {
        let mut pool_nodes = Vec::with_capacity(MAX_POOL_NODES);
        for _ in 0..MAX_POOL_NODES {
            pool_nodes.push(PoolNode::new());
        }
        Arc::new(Self {
            device: Some(device),
            mutex: Mutex::new(PoolInner::new()),
            command_buffers_set: VulkanCommandBuffersSet::new(),
            semaphore_set: VulkanSemaphoreSet::new(),
            fence_set: VulkanFenceSet::new(),
            query_pool_set: VulkanQueryPoolSet::new(),
            pool_nodes: UnsafeCell::new(pool_nodes),
        })
    }

    /// Configure the pool, creating command buffers, fences, semaphores, and
    /// optionally a query pool.
    /// Mirrors C++ `VulkanCommandBufferPool::Configure`.
    ///
    /// # Safety
    /// The `device` must be valid. If `create_query_pool` is true, `p_next`
    /// must point to a valid Vulkan structure chain (e.g., video profile).
    pub unsafe fn configure(
        self: &mut Arc<Self>,
        device: &vulkanalia::Device,
        num_pool_nodes: u32,
        queue_family_index: u32,
        create_query_pool: bool,
        next: *const std::ffi::c_void,
        create_semaphores: bool,
        create_fences: bool,
    ) -> vk::Result {
        let pool = Arc::get_mut(self)
            .expect("Cannot configure pool while other references exist");

        let mut inner = pool.mutex.lock().unwrap();

        let nodes = pool.pool_nodes.get_mut();
        if num_pool_nodes as usize > nodes.len() {
            return vk::Result::ERROR_TOO_MANY_OBJECTS;
        }

        let result = pool.command_buffers_set.create_command_buffer_pool(
            device,
            queue_family_index,
            num_pool_nodes,
        );
        if result != vk::Result::SUCCESS {
            return result;
        }

        if create_semaphores {
            let result = pool.semaphore_set.create_set(device, num_pool_nodes);
            if result != vk::Result::SUCCESS {
                return result;
            }
        }

        if create_fences {
            let result = pool.fence_set.create_set(device, num_pool_nodes);
            if result != vk::Result::SUCCESS {
                return result;
            }
        }

        if create_query_pool {
            let _result = pool.query_pool_set.create_set(
                device,
                num_pool_nodes,
                vk::QueryType::VIDEO_ENCODE_FEEDBACK_KHR,
                vk::QueryPoolCreateFlags::empty(),
                next,
            );
        }

        for i in 0..num_pool_nodes {
            let _ = nodes[i as usize].init(device.clone());
            inner.available_pool_nodes |= 1u64 << i;
        }

        pool.device = Some(device.clone());
        inner.pool_size = num_pool_nodes;
        inner.queue_family_index = queue_family_index;
        vk::Result::SUCCESS
    }

    /// Acquire an available pool node.
    /// Mirrors C++ `VulkanCommandBufferPool::GetAvailablePoolNode`.
    ///
    /// Returns the pool node index if one is available.
    pub fn get_available_pool_node(self: &Arc<Self>) -> Option<PoolNodeHandle> {
        let available_index = {
            let mut inner = self.mutex.lock().unwrap();
            if inner.next_node_to_use >= inner.pool_size {
                inner.next_node_to_use = 0;
            }

            let mut found: Option<u32> = None;
            let mut retry = false;

            loop {
                for i in inner.next_node_to_use..inner.pool_size {
                    if inner.available_pool_nodes & (1u64 << i) != 0 {
                        inner.next_node_to_use = i + 1;
                        inner.available_pool_nodes &= !(1u64 << i);
                        found = Some(i);
                        break;
                    }
                }

                if found.is_none() && inner.next_node_to_use > 0 {
                    inner.next_node_to_use = 0;
                    if !retry {
                        retry = true;
                        continue;
                    }
                }
                break;
            }

            found
        };

        available_index.map(|idx| {
            // SAFETY: We have exclusive logical ownership of this node (bit cleared).
            // We need a mutable reference to the pool node. Since the pool is behind
            // Arc, we cast away constness here. This is safe because the bitmask
            // guarantees no two handles reference the same node concurrently.
            // SAFETY: Bitmask guarantees exclusive logical ownership of this node index.
            unsafe {
                let nodes = &mut *self.pool_nodes.get();
                nodes[idx as usize].set_parent(idx as i32);
            }
            PoolNodeHandle {
                pool: Arc::clone(self),
                index: idx,
            }
        })
    }

    /// Release a pool node back to the available set.
    /// Mirrors C++ `VulkanCommandBufferPool::ReleasePoolNodeToPool`.
    pub fn release_pool_node_to_pool(&self, pool_node_index: u32) -> bool {
        let mut inner = self.mutex.lock().unwrap();
        debug_assert!(
            inner.available_pool_nodes & (1u64 << pool_node_index) == 0,
            "Pool node {} is already marked as available",
            pool_node_index
        );
        inner.available_pool_nodes |= 1u64 << pool_node_index;
        true
    }

    /// Get the command buffer for a given pool index.
    pub fn get_command_buffer(&self, index: u32) -> Option<vk::CommandBuffer> {
        self.command_buffers_set.get_command_buffer(index)
    }

    /// Get the fence for a given pool index.
    pub fn get_fence(&self, index: u32) -> vk::Fence {
        self.fence_set.get_fence(index)
    }

    /// Get the semaphore for a given pool index.
    pub fn get_semaphore(&self, index: u32) -> vk::Semaphore {
        self.semaphore_set.get_semaphore(index)
    }

    /// Get the query pool for a given query index.
    pub fn get_query_pool(&self, query_idx: u32) -> vk::QueryPool {
        self.query_pool_set.get_query_pool(query_idx)
    }

    /// Access pool node by index.
    pub fn pool_node(&self, index: u32) -> &PoolNode {
        // SAFETY: Caller must ensure no mutable access to this node is active.
        unsafe {
            let nodes = &*self.pool_nodes.get();
            &nodes[index as usize]
        }
    }

    /// Get the total number of pool node slots.
    pub fn size(&self) -> usize {
        // SAFETY: len() is a read-only operation.
        unsafe {
            let nodes = &*self.pool_nodes.get();
            nodes.len()
        }
    }

    /// Deinitialize all pool nodes.
    /// Mirrors C++ `VulkanCommandBufferPool::Deinit`.
    ///
    /// # Safety
    /// All GPU work using pool resources must be complete.
    pub unsafe fn deinit(self: &mut Arc<Self>) {
        let pool = Arc::get_mut(self)
            .expect("Cannot deinit pool while other references exist");
        let inner = pool.mutex.lock().unwrap();
        let pool_size = inner.pool_size as usize;
        drop(inner);

        let nodes = pool.pool_nodes.get_mut();
        for i in 0..pool_size {
            nodes[i].deinit();
        }

        pool.command_buffers_set.destroy();
        pool.semaphore_set.destroy();
        pool.fence_set.destroy();
        pool.query_pool_set.destroy();
    }
}

// ---------------------------------------------------------------------------
// PoolNodeHandle — RAII wrapper for checked-out pool nodes
// ---------------------------------------------------------------------------

/// RAII handle to a checked-out pool node. When dropped, the node is released
/// back to the pool. This replaces the C++ shared_ptr-with-custom-deleter
/// pattern used in `GetAvailablePoolNode`.
pub struct PoolNodeHandle {
    pool: Arc<VulkanCommandBufferPool>,
    index: u32,
}

impl PoolNodeHandle {
    /// Get the pool node index.
    pub fn index(&self) -> u32 {
        self.index
    }

    /// Get the command buffer for this node.
    pub fn get_command_buffer(&self) -> Option<vk::CommandBuffer> {
        self.pool.get_command_buffer(self.index)
    }

    /// Get the fence for this node.
    pub fn get_fence(&self) -> vk::Fence {
        self.pool.get_fence(self.index)
    }

    /// Get the semaphore for this node.
    pub fn get_semaphore(&self) -> vk::Semaphore {
        self.pool.get_semaphore(self.index)
    }

    /// Get the query pool and query index for this node.
    pub fn get_query_pool(&self) -> (vk::QueryPool, u32) {
        (self.pool.get_query_pool(self.index), self.index)
    }

    /// Access the underlying pool node for state mutation.
    ///
    /// # Safety
    /// The caller must not hold multiple mutable references to the same node.
    /// This is safe in practice because `PoolNodeHandle` has exclusive ownership
    /// of the node index (guaranteed by the bitmask).
    pub unsafe fn node_mut(&self) -> &mut PoolNode {
        let nodes = &mut *self.pool.pool_nodes.get();
        &mut nodes[self.index as usize]
    }

    /// Begin command buffer recording.
    ///
    /// # Safety
    /// The command buffer must be in reset state.
    pub unsafe fn begin_command_buffer_recording(
        &self,
        begin_info: &vk::CommandBufferBeginInfo,
    ) -> Result<vk::CommandBuffer, vk::Result> {
        let cmd_buf = self
            .get_command_buffer()
            .ok_or(vk::Result::ERROR_INITIALIZATION_FAILED)?;
        self.node_mut()
            .begin_command_buffer_recording(cmd_buf, begin_info)
    }

    /// End command buffer recording.
    ///
    /// # Safety
    /// The command buffer must be in recording state.
    pub unsafe fn end_command_buffer_recording(&self, cmd_buf: vk::CommandBuffer) -> vk::Result {
        self.node_mut().end_command_buffer_recording(cmd_buf)
    }

    /// Mark the command buffer as submitted.
    pub fn set_command_buffer_submitted(&self) -> bool {
        // SAFETY: We have exclusive logical ownership of this node.
        unsafe { self.node_mut().set_command_buffer_submitted() }
    }

    /// Wait on the host for command buffer completion.
    ///
    /// # Safety
    /// The fence must have been submitted.
    pub unsafe fn sync_host_on_cmd_buff_complete(
        &self,
        reset_after_wait: bool,
        fence_name: &str,
    ) -> vk::Result {
        let fence = self.get_fence();
        self.node_mut().sync_host_on_cmd_buff_complete(
            fence,
            reset_after_wait,
            fence_name,
            DEFAULT_FENCE_WAIT_TIMEOUT_NSEC,
            DEFAULT_FENCE_TOTAL_WAIT_TIMEOUT_NSEC,
        )
    }

    /// Reset the command buffer, optionally waiting for GPU completion first.
    ///
    /// # Safety
    /// If `sync_with_host` is true, the command buffer must have been submitted.
    pub unsafe fn reset_command_buffer(&self, sync_with_host: bool, fence_name: &str) -> bool {
        let fence = self.get_fence();
        self.node_mut()
            .reset_command_buffer(fence, sync_with_host, fence_name)
    }

    /// Get the command buffer state.
    pub fn cmd_buf_state(&self) -> CmdBufState {
        // SAFETY: Reading an enum is safe; we own this node exclusively.
        unsafe { self.node_mut().cmd_buf_state() }
    }
}

impl Drop for PoolNodeHandle {
    fn drop(&mut self) {
        // SAFETY: We have exclusive logical ownership via the bitmask.
        unsafe {
            let nodes = &mut *self.pool.pool_nodes.get();
            nodes[self.index as usize].clear_parent();
        }
        self.pool.release_pool_node_to_pool(self.index);
    }
}

// ---------------------------------------------------------------------------
// PoolNodeHandler — RAII recording helper
// ---------------------------------------------------------------------------

/// RAII wrapper for command buffer recording with automatic lifecycle
/// management. Mirrors C++ `VulkanCommandBufferPool::PoolNodeHandler`.
///
/// Usage:
/// ```ignore
/// let mut handler = PoolNodeHandler::new(&pool, "my_operation", false);
/// if let Some(cmd_buf) = handler.command_buffer() {
///     // Record commands ...
///     handler.end_cmd_buffer_recording();
///     // Submit ...
/// }
/// // Automatic SetCommandBufferSubmitted + optional SyncHostOnCmdBuffComplete on drop.
/// ```
pub struct PoolNodeHandler {
    node_handle: Option<PoolNodeHandle>,
    cmd_buf: vk::CommandBuffer,
    operation_name: String,
    wait_on_cpu_after_submit: bool,
    command_ended: bool,
}

impl PoolNodeHandler {
    /// Create a new handler, acquiring a pool node and beginning recording.
    ///
    /// # Safety
    /// The pool must be configured and the device valid.
    pub unsafe fn new(
        pool: &Arc<VulkanCommandBufferPool>,
        operation_name: &str,
        wait_on_cpu_after_submit: bool,
    ) -> Self {
        let node_handle = pool.get_available_pool_node();
        let mut cmd_buf = vk::CommandBuffer::null();

        if let Some(ref handle) = node_handle {
            handle.reset_command_buffer(true, operation_name);

            let begin_info = vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);

            match handle.begin_command_buffer_recording(&begin_info) {
                Ok(buf) => cmd_buf = buf,
                Err(_) => {}
            }
        }

        Self {
            node_handle,
            cmd_buf,
            operation_name: operation_name.to_string(),
            wait_on_cpu_after_submit,
            command_ended: false,
        }
    }

    /// Create a handler with an existing pool node handle.
    ///
    /// # Safety
    /// The command buffer must be in a valid state for reset + begin.
    pub unsafe fn with_existing_node(
        pool: &Arc<VulkanCommandBufferPool>,
        existing_node: Option<PoolNodeHandle>,
        operation_name: &str,
        wait_on_cpu_after_submit: bool,
    ) -> Self {
        let node_handle = existing_node.or_else(|| pool.get_available_pool_node());
        let mut cmd_buf = vk::CommandBuffer::null();

        if let Some(ref handle) = node_handle {
            handle.reset_command_buffer(true, operation_name);

            let begin_info = vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);

            match handle.begin_command_buffer_recording(&begin_info) {
                Ok(buf) => cmd_buf = buf,
                Err(_) => {}
            }
        }

        Self {
            node_handle,
            cmd_buf,
            operation_name: operation_name.to_string(),
            wait_on_cpu_after_submit,
            command_ended: false,
        }
    }

    /// End command buffer recording.
    /// Mirrors C++ `PoolNodeHandler::EndCmdBufferRecording`.
    ///
    /// # Safety
    /// The command buffer must be in recording state.
    pub unsafe fn end_cmd_buffer_recording(&mut self) -> vk::Result {
        if let Some(ref handle) = self.node_handle {
            if self.cmd_buf == vk::CommandBuffer::null() {
                return vk::Result::ERROR_INITIALIZATION_FAILED;
            }
            let result = handle.end_command_buffer_recording(self.cmd_buf);
            if result == vk::Result::SUCCESS {
                self.command_ended = true;
            }
            result
        } else {
            vk::Result::ERROR_INITIALIZATION_FAILED
        }
    }

    /// Check if the handler is valid and has a command buffer ready for recording.
    pub fn is_valid(&self) -> bool {
        self.cmd_buf != vk::CommandBuffer::null()
    }

    /// Get the command buffer for recording commands.
    pub fn command_buffer(&self) -> Option<vk::CommandBuffer> {
        if self.cmd_buf != vk::CommandBuffer::null() {
            Some(self.cmd_buf)
        } else {
            None
        }
    }

    /// Get the fence for submission.
    pub fn get_fence(&self) -> vk::Fence {
        self.node_handle
            .as_ref()
            .map(|h| h.get_fence())
            .unwrap_or(vk::Fence::null())
    }

    /// Get the semaphore for submission.
    pub fn get_semaphore(&self) -> vk::Semaphore {
        self.node_handle
            .as_ref()
            .map(|h| h.get_semaphore())
            .unwrap_or(vk::Semaphore::null())
    }

    /// Get a reference to the underlying pool node handle.
    pub fn node_handle(&self) -> Option<&PoolNodeHandle> {
        self.node_handle.as_ref()
    }

    /// Take ownership of the pool node handle out of this handler.
    /// The handler will no longer perform automatic cleanup for it.
    pub fn take_node_handle(&mut self) -> Option<PoolNodeHandle> {
        self.node_handle.take()
    }
}

impl Drop for PoolNodeHandler {
    fn drop(&mut self) {
        if let Some(ref handle) = self.node_handle {
            if self.command_ended {
                handle.set_command_buffer_submitted();

                if self.wait_on_cpu_after_submit {
                    // SAFETY: If we got here, the command buffer was recorded and
                    // submitted, so the fence is valid.
                    unsafe {
                        handle.sync_host_on_cmd_buff_complete(false, &self.operation_name);
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Test that `CmdBufState` has the expected values and transitions.
    #[test]
    fn test_cmd_buf_state_values() {
        assert_eq!(CmdBufState::Reset as u32, 0);
        assert_eq!(CmdBufState::Recording as u32, 1);
        assert_eq!(CmdBufState::Recorded as u32, 2);
        assert_eq!(CmdBufState::Submitted as u32, 3);
    }

    /// Test pool node default construction.
    #[test]
    fn test_pool_node_default() {
        let node = PoolNode::new();
        assert_eq!(node.cmd_buf_state(), CmdBufState::Reset);
        assert_eq!(node.parent_index(), None);
        assert!(!node.is_valid());
    }

    /// Test pool node parent assignment and clearing.
    #[test]
    fn test_pool_node_parent_lifecycle() {
        let mut node = PoolNode::new();

        // Set parent
        let result = node.set_parent(5);
        assert_eq!(result, vk::Result::SUCCESS);
        assert_eq!(node.parent_index(), Some(5));
        assert_eq!(node.get_node_pool_index(), Some(5));

        // Clear parent — state must NOT be reset (matches C++ behavior).
        node.clear_parent();
        assert_eq!(node.parent_index(), None);
        // State persists across clear_parent:
        assert_eq!(node.cmd_buf_state(), CmdBufState::Reset);
    }

    /// Test that clear_parent preserves cmd_buf_state (critical for fence
    /// management, as noted in the C++ source).
    #[test]
    fn test_clear_parent_preserves_state() {
        let mut node = PoolNode::new();
        node.set_parent(0);

        // Manually advance state to Submitted
        node.cmd_buf_state = CmdBufState::Submitted;

        // ClearParent must NOT reset state
        node.clear_parent();
        assert_eq!(node.cmd_buf_state(), CmdBufState::Submitted);
    }

    /// Test the available_pool_nodes bitmask logic used in
    /// `get_available_pool_node` / `release_pool_node_to_pool`.
    #[test]
    fn test_available_bitmask_operations() {
        let mut available: u64 = 0;
        let pool_size: u32 = 8;

        // Mark all nodes as available (simulates Configure)
        for i in 0..pool_size {
            available |= 1u64 << i;
        }
        assert_eq!(available, 0xFF);

        // Acquire node 0
        let idx = find_available_node(&mut available, pool_size, 0);
        assert_eq!(idx, Some(0));
        assert_eq!(available, 0xFE);

        // Acquire node 1
        let idx = find_available_node(&mut available, pool_size, 1);
        assert_eq!(idx, Some(1));
        assert_eq!(available, 0xFC);

        // Release node 0
        available |= 1u64 << 0;
        assert_eq!(available, 0xFD);

        // Acquire starting from 2 — should get 2
        let idx = find_available_node(&mut available, pool_size, 2);
        assert_eq!(idx, Some(2));

        // Acquire 5 more (nodes 0,3,4,5,6 — 0 was re-released above)
        for _ in 0..5 {
            let _ = find_available_node(&mut available, pool_size, 0);
        }
        // Only node 7 remains
        let idx = find_available_node(&mut available, pool_size, 0);
        assert_eq!(idx, Some(7));

        // Now truly empty
        let idx = find_available_node(&mut available, pool_size, 0);
        assert_eq!(idx, None);
    }

    /// Test wraparound behavior: when next_node_to_use is past the end,
    /// it should wrap around to find available nodes at the beginning.
    #[test]
    fn test_wraparound_search() {
        let mut available: u64 = 0;
        let pool_size: u32 = 4;

        // Only node 0 and 1 are available.
        available |= 1u64 << 0;
        available |= 1u64 << 1;

        // Start searching from index 3 (past the available ones).
        let mut next = 3u32;
        let idx = find_available_node_with_wrap(&mut available, pool_size, &mut next);
        assert_eq!(idx, Some(0));
        assert_eq!(next, 1); // next_node_to_use advanced past found node
    }

    /// Test MAX_POOL_NODES constant matches C++.
    #[test]
    fn test_max_pool_nodes() {
        assert_eq!(MAX_POOL_NODES, 64);
        // The bitmask is u64, so 64 nodes is the max.
        let all_set: u64 = u64::MAX;
        assert_eq!(all_set.count_ones(), 64);
    }

    /// Test PoolInner default state.
    #[test]
    fn test_pool_inner_default() {
        let inner = PoolInner::new();
        assert_eq!(inner.pool_size, 0);
        assert_eq!(inner.next_node_to_use, 0);
        assert_eq!(inner.available_pool_nodes, 0);
        assert_eq!(inner.queue_family_index, u32::MAX);
    }

    /// Test set_command_buffer_submitted state transition requirements.
    #[test]
    fn test_set_command_buffer_submitted_requires_recorded() {
        let mut node = PoolNode::new();
        node.set_parent(0);
        // No device — is_valid will be false. But let's test state logic only:
        // Without a device, set_command_buffer_submitted returns false.
        assert!(!node.set_command_buffer_submitted());

        // Even with state set to Recorded, without device it fails.
        node.cmd_buf_state = CmdBufState::Recorded;
        assert!(!node.set_command_buffer_submitted());
    }

    // -- Helper functions for testing bitmask logic extracted from the pool --

    /// Simulates the core bitmask search from `get_available_pool_node`,
    /// starting from `start` and searching `[start..pool_size)`.
    fn find_available_node(available: &mut u64, pool_size: u32, start: u32) -> Option<u32> {
        for i in start..pool_size {
            if *available & (1u64 << i) != 0 {
                *available &= !(1u64 << i);
                return Some(i);
            }
        }
        // Retry from 0
        for i in 0..start {
            if *available & (1u64 << i) != 0 {
                *available &= !(1u64 << i);
                return Some(i);
            }
        }
        None
    }

    /// Simulates the full wraparound search including next_node_to_use update.
    fn find_available_node_with_wrap(
        available: &mut u64,
        pool_size: u32,
        next_node_to_use: &mut u32,
    ) -> Option<u32> {
        if *next_node_to_use >= pool_size {
            *next_node_to_use = 0;
        }

        let mut found: Option<u32> = None;
        let mut retry = false;

        loop {
            for i in *next_node_to_use..pool_size {
                if *available & (1u64 << i) != 0 {
                    *next_node_to_use = i + 1;
                    *available &= !(1u64 << i);
                    found = Some(i);
                    break;
                }
            }

            if found.is_none() && *next_node_to_use > 0 {
                *next_node_to_use = 0;
                if !retry {
                    retry = true;
                    continue;
                }
            }
            break;
        }

        found
    }
}

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of VkParserVideoPictureParameters.h + VkParserVideoPictureParameters.cpp
//!
//! Manages VkVideoSessionParametersKHR for SPS/PPS parameter sets.
//! Handles add/update of H.264, H.265, and AV1 parameter sets.

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;

// Placeholder import — will be used when codec_utils types are fully integrated.
#[allow(unused_imports)]
use crate::codec_utils;

// ---------------------------------------------------------------------------
// StdVideoPictureParametersSet types (placeholder — lives in codec_utils)
// These mirror the C++ StdVideoPictureParametersSet enums and trait.
// ---------------------------------------------------------------------------

/// The kind of standard-video parameter set (codec-specific).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdType {
    H264Sps,
    H264Pps,
    H265Vps,
    H265Sps,
    H265Pps,
    Av1Sps,
}

/// Higher-level categorization used for bitset tracking and hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParameterType {
    Invalid,
    PpsType,
    SpsType,
    VpsType,
    Av1SpsType,
    /// Total number of non-invalid types (used for array sizing).
    NumOfTypes,
}

impl ParameterType {
    pub const COUNT: usize = 5; // PPS, SPS, VPS, AV1_SPS, NumOfTypes sentinel
}

/// Placeholder trait for picture parameter set objects coming from the parser.
/// The real implementation lives in `codec_utils::std_video_picture_parameters_set`.
pub trait StdVideoPictureParametersSetIf: Send + Sync {
    fn get_std_type(&self) -> StdType;
    fn get_parameter_type(&self) -> ParameterType;
    fn get_update_sequence_count(&self) -> u32;

    fn get_vps_id(&self) -> (i32, bool);
    fn get_sps_id(&self) -> (i32, bool);
    fn get_pps_id(&self) -> (i32, bool);

    /// Release the client-side back-reference (shared_ptr break).
    fn release_client_object(&self);
    /// Get the client object (our VkParserVideoPictureParameters) if set.
    fn get_client_object(&self) -> Option<Arc<dyn std::any::Any + Send + Sync>>;

    // Codec-specific raw pointers (opaque to this module).
    fn get_std_h264_sps(&self) -> *const std::ffi::c_void { std::ptr::null() }
    fn get_std_h264_pps(&self) -> *const std::ffi::c_void { std::ptr::null() }
    fn get_std_h265_vps(&self) -> *const std::ffi::c_void { std::ptr::null() }
    fn get_std_h265_sps(&self) -> *const std::ffi::c_void { std::ptr::null() }
    fn get_std_h265_pps(&self) -> *const std::ffi::c_void { std::ptr::null() }
    fn get_std_av1_sps(&self) -> *const std::ffi::c_void { std::ptr::null() }
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const MAX_VPS_IDS: usize = 16;
pub const MAX_SPS_IDS: usize = 32;
pub const MAX_PPS_IDS: usize = 256;

const REF_CLASS_ID: &str = "VkParserVideoPictureParameters";

/// Global monotonically-increasing ID counter (mirrors `m_currentId`).
static CURRENT_ID: AtomicI32 = AtomicI32::new(0);

// ---------------------------------------------------------------------------
// Bitset helper — fixed-size bit array (replaces std::bitset)
// ---------------------------------------------------------------------------

macro_rules! define_bitset {
    ($name:ident, $capacity:expr) => {
        #[derive(Clone)]
        struct $name {
            words: [u64; ($capacity + 63) / 64],
        }

        impl $name {
            const CAPACITY: usize = $capacity;

            fn new() -> Self {
                Self {
                    words: [0u64; ($capacity + 63) / 64],
                }
            }

            fn set(&mut self, idx: usize, value: bool) {
                debug_assert!(idx < Self::CAPACITY, "bitset index out of bounds");
                let word = idx / 64;
                let bit = idx % 64;
                if value {
                    self.words[word] |= 1u64 << bit;
                } else {
                    self.words[word] &= !(1u64 << bit);
                }
            }

            fn get(&self, idx: usize) -> bool {
                debug_assert!(idx < Self::CAPACITY, "bitset index out of bounds");
                let word = idx / 64;
                let bit = idx % 64;
                (self.words[word] & (1u64 << bit)) != 0
            }

            fn clear(&mut self) {
                for w in self.words.iter_mut() {
                    *w = 0;
                }
            }
        }
    };
}

define_bitset!(VpsBitSet, MAX_VPS_IDS);
define_bitset!(SpsBitSet, MAX_SPS_IDS);
define_bitset!(PpsBitSet, MAX_PPS_IDS);
define_bitset!(Av1SpsBitSet, MAX_SPS_IDS);

// ---------------------------------------------------------------------------
// VkParserVideoPictureParameters
// ---------------------------------------------------------------------------

/// Manages `VkVideoSessionParametersKHR` objects for decode picture parameter
/// sets. Mirrors the C++ `VkParserVideoPictureParameters` class.
///
/// # Divergence from C++
/// - No ref-counting base class; use `Arc<VkParserVideoPictureParameters>` for
///   shared ownership.
/// - `VkSharedBaseObj` replaced by `Arc` / `Option<Arc<...>>`.
/// - Vulkan device context represented as opaque `*const ()` placeholder;
///   real integration will use `vulkanalia::Device`.
pub struct VkParserVideoPictureParameters {
    class_id: &'static str,
    id: i32,
    // TODO: Replace with real vulkanalia::Device + function pointers when integrating.
    _vk_dev_ctx: *const (),
    video_session: Option<vk::VideoSessionKHR>,
    session_parameters: vk::VideoSessionParametersKHR,
    vps_ids_used: VpsBitSet,
    sps_ids_used: SpsBitSet,
    pps_ids_used: PpsBitSet,
    av1_sps_ids_used: Av1SpsBitSet,
    template_picture_parameters: Option<Arc<VkParserVideoPictureParameters>>,
    update_count: u32,
    picture_parameters_queue: VecDeque<Arc<dyn StdVideoPictureParametersSetIf>>,
    last_pict_params_queue: [Option<Arc<dyn StdVideoPictureParametersSetIf>>; ParameterType::COUNT],
    all_registered_params: Vec<Arc<dyn StdVideoPictureParametersSetIf>>,
}

// Safety: The raw pointer `_vk_dev_ctx` is only used as an opaque handle.
unsafe impl Send for VkParserVideoPictureParameters {}
unsafe impl Sync for VkParserVideoPictureParameters {}

impl VkParserVideoPictureParameters {
    // ------------------------------------------------------------------
    // Construction
    // ------------------------------------------------------------------

    fn new(
        vk_dev_ctx: *const (),
        template: Option<Arc<VkParserVideoPictureParameters>>,
    ) -> Self {
        Self {
            class_id: REF_CLASS_ID,
            id: -1,
            _vk_dev_ctx: vk_dev_ctx,
            video_session: None,
            session_parameters: vk::VideoSessionParametersKHR::null(),
            vps_ids_used: VpsBitSet::new(),
            sps_ids_used: SpsBitSet::new(),
            pps_ids_used: PpsBitSet::new(),
            av1_sps_ids_used: Av1SpsBitSet::new(),
            template_picture_parameters: template,
            update_count: 0,
            picture_parameters_queue: VecDeque::new(),
            last_pict_params_queue: Default::default(),
            all_registered_params: Vec::new(),
        }
    }

    /// Static factory — mirrors `VkParserVideoPictureParameters::Create`.
    pub fn create(
        vk_dev_ctx: *const (),
        template: Option<Arc<VkParserVideoPictureParameters>>,
    ) -> Result<Arc<Self>, vk::Result> {
        Ok(Arc::new(Self::new(vk_dev_ctx, template)))
    }

    /// Cast from an opaque base reference. Mirrors `VideoPictureParametersFromBase`.
    pub fn from_base(base: &VkParserVideoPictureParameters) -> Option<&VkParserVideoPictureParameters> {
        if base.class_id == REF_CLASS_ID {
            Some(base)
        } else {
            debug_assert!(false, "Invalid VkParserVideoPictureParameters from base");
            None
        }
    }

    // ------------------------------------------------------------------
    // Accessors
    // ------------------------------------------------------------------

    pub fn get_video_session_parameters_khr(&self) -> vk::VideoSessionParametersKHR {
        debug_assert!(
            self.session_parameters != vk::VideoSessionParametersKHR::null(),
            "session_parameters is null"
        );
        self.session_parameters
    }

    pub fn get_id(&self) -> i32 {
        self.id
    }

    pub fn has_vps_id(&self, vps_id: u32) -> bool {
        self.vps_ids_used.get(vps_id as usize)
    }

    pub fn has_sps_id(&self, sps_id: u32) -> bool {
        self.sps_ids_used.get(sps_id as usize)
    }

    pub fn has_pps_id(&self, pps_id: u32) -> bool {
        self.pps_ids_used.get(pps_id as usize)
    }

    pub fn has_av1_pps_id(&self, pps_id: u32) -> bool {
        self.av1_sps_ids_used.get(pps_id as usize)
    }

    // ------------------------------------------------------------------
    // Populate helpers (static)
    // ------------------------------------------------------------------

    /// Populate H.264 session parameters add info from a picture parameter set.
    /// Returns the SPS or PPS id, or -1 on failure.
    ///
    /// Mirrors `PopulateH264UpdateFields`.
    pub fn populate_h264_update_fields(
        pps_set: &dyn StdVideoPictureParametersSetIf,
    ) -> i32 {
        let std_type = pps_set.get_std_type();
        debug_assert!(
            std_type == StdType::H264Sps || std_type == StdType::H264Pps,
            "Incorrect H.264 type"
        );

        match std_type {
            StdType::H264Sps => {
                let (id, is_sps) = pps_set.get_sps_id();
                debug_assert!(is_sps);
                // In a real build the caller would fill the Vulkan struct fields:
                // h264_add_info.std_sp_s_count = 1;
                // h264_add_info.p_std_sp_ss = pps_set.get_std_h264_sps();
                id
            }
            StdType::H264Pps => {
                let (id, is_pps) = pps_set.get_pps_id();
                debug_assert!(is_pps);
                id
            }
            _ => {
                debug_assert!(false, "Incorrect H.264 type");
                -1
            }
        }
    }

    /// Populate H.265 session parameters add info from a picture parameter set.
    /// Returns the VPS, SPS, or PPS id, or -1 on failure.
    ///
    /// Mirrors `PopulateH265UpdateFields`.
    pub fn populate_h265_update_fields(
        pps_set: &dyn StdVideoPictureParametersSetIf,
    ) -> i32 {
        let std_type = pps_set.get_std_type();
        debug_assert!(
            std_type == StdType::H265Vps
                || std_type == StdType::H265Sps
                || std_type == StdType::H265Pps,
            "Incorrect H.265 type"
        );

        match std_type {
            StdType::H265Vps => {
                let (id, is_vps) = pps_set.get_vps_id();
                debug_assert!(is_vps);
                id
            }
            StdType::H265Sps => {
                let (id, is_sps) = pps_set.get_sps_id();
                debug_assert!(is_sps);
                id
            }
            StdType::H265Pps => {
                let (id, is_pps) = pps_set.get_pps_id();
                debug_assert!(is_pps);
                id
            }
            _ => {
                debug_assert!(false, "Incorrect H.265 type");
                -1
            }
        }
    }

    // ------------------------------------------------------------------
    // Create / Update parameters object
    // ------------------------------------------------------------------

    /// Create a new `VkVideoSessionParametersKHR` object.
    ///
    /// Mirrors `CreateParametersObject`. In a real build this calls
    /// `vkCreateVideoSessionParametersKHR` via the device context.
    ///
    /// # Placeholder
    /// The actual Vulkan calls are stubbed out — only the bookkeeping logic
    /// (bitset tracking, ID assignment) is fully ported.
    pub fn create_parameters_object(
        &mut self,
        _vk_dev_ctx: *const (),
        video_session: vk::VideoSessionKHR,
        pps_set: &dyn StdVideoPictureParametersSetIf,
        template: Option<&VkParserVideoPictureParameters>,
    ) -> vk::Result {
        let update_type = pps_set.get_std_type();
        let current_id: i32;

        match update_type {
            StdType::H264Sps | StdType::H264Pps => {
                current_id = Self::populate_h264_update_fields(pps_set);
            }
            StdType::H265Vps | StdType::H265Sps | StdType::H265Pps => {
                current_id = Self::populate_h265_update_fields(pps_set);
            }
            StdType::Av1Sps => {
                // AV1 does not support template parameters (VUID-09258).
                // template is forced to None for AV1.
                current_id = 0;
            }
        }

        // TODO: Actually call vkCreateVideoSessionParametersKHR here.
        // For now, simulate success.
        let result = vk::Result::SUCCESS;

        if result == vk::Result::SUCCESS {
            self.video_session = Some(video_session);

            // Copy bitsets from template if present (and not AV1).
            let effective_template = if update_type == StdType::Av1Sps {
                None
            } else {
                template
            };

            if let Some(tmpl) = effective_template {
                self.vps_ids_used = tmpl.vps_ids_used.clone();
                self.sps_ids_used = tmpl.sps_ids_used.clone();
                self.pps_ids_used = tmpl.pps_ids_used.clone();
                self.av1_sps_ids_used = tmpl.av1_sps_ids_used.clone();
            }

            debug_assert!(current_id >= 0);
            match pps_set.get_parameter_type() {
                ParameterType::PpsType => self.pps_ids_used.set(current_id as usize, true),
                ParameterType::SpsType => self.sps_ids_used.set(current_id as usize, true),
                ParameterType::VpsType => self.vps_ids_used.set(current_id as usize, true),
                ParameterType::Av1SpsType => self.av1_sps_ids_used.set(current_id as usize, true),
                _ => debug_assert!(false, "Invalid parameter type"),
            }

            self.id = CURRENT_ID.fetch_add(1, Ordering::SeqCst) + 1;
        } else {
            debug_assert!(false, "Could not create Session Parameters Object");
        }

        result
    }

    /// Update an existing `VkVideoSessionParametersKHR` with new parameter data.
    ///
    /// Mirrors `UpdateParametersObject`.
    pub fn update_parameters_object(
        &mut self,
        pps_set: &dyn StdVideoPictureParametersSetIf,
    ) -> vk::Result {
        let update_type = pps_set.get_std_type();
        let current_id: i32;

        match update_type {
            StdType::H264Sps | StdType::H264Pps => {
                current_id = Self::populate_h264_update_fields(pps_set);
            }
            StdType::H265Vps | StdType::H265Sps | StdType::H265Pps => {
                current_id = Self::populate_h265_update_fields(pps_set);
            }
            StdType::Av1Sps => {
                debug_assert!(false, "No calls to UpdateParametersObject for AV1");
                return vk::Result::SUCCESS;
            }
        }

        self.update_count += 1;

        // TODO: Actually call vkUpdateVideoSessionParametersKHR here.
        // Simulate success for now.
        let result = vk::Result::SUCCESS;

        if result != vk::Result::SUCCESS {
            // Rollback counter on failure.
            self.update_count -= 1;
            debug_assert!(false, "Could not update Session Parameters Object");
        } else {
            debug_assert!(current_id >= 0);
            match pps_set.get_parameter_type() {
                ParameterType::PpsType => self.pps_ids_used.set(current_id as usize, true),
                ParameterType::SpsType => self.sps_ids_used.set(current_id as usize, true),
                ParameterType::VpsType => self.vps_ids_used.set(current_id as usize, true),
                ParameterType::Av1SpsType => self.av1_sps_ids_used.set(current_id as usize, true),
                _ => debug_assert!(false, "Invalid parameter type"),
            }
        }

        result
    }

    // ------------------------------------------------------------------
    // Queue management
    // ------------------------------------------------------------------

    /// Add a picture parameter set to the pending queue.
    ///
    /// Mirrors `AddPictureParametersToQueue`.
    pub fn add_picture_parameters_to_queue(
        &mut self,
        pps_set: Arc<dyn StdVideoPictureParametersSetIf>,
    ) -> vk::Result {
        self.picture_parameters_queue.push_back(pps_set.clone());
        self.all_registered_params.push(pps_set);
        vk::Result::SUCCESS
    }

    /// Handle a newly-arrived picture parameter set — either create or update
    /// the session parameters object.
    ///
    /// Mirrors `HandleNewPictureParametersSet`.
    pub fn handle_new_picture_parameters_set(
        &mut self,
        video_session: vk::VideoSessionKHR,
        pps_set: &dyn StdVideoPictureParametersSetIf,
    ) -> vk::Result {
        if self.session_parameters == vk::VideoSessionParametersKHR::null() {
            debug_assert!(video_session != vk::VideoSessionKHR::null());
            debug_assert!(self.video_session.is_none());

            // Flush template if present.
            // NOTE: In the real build, `flush_picture_parameters_queue` on the
            // template requires mutable access. This would need interior
            // mutability (Mutex) in production code.

            // Clone the Arc to avoid borrowing self while calling a &mut self method.
            let template_clone = self.template_picture_parameters.clone();
            let template_ref = template_clone.as_deref();
            let result = self.create_parameters_object(
                self._vk_dev_ctx,
                video_session,
                pps_set,
                template_ref,
            );
            debug_assert!(result == vk::Result::SUCCESS);
            self.video_session = Some(video_session);
            result
        } else {
            debug_assert!(self.video_session.is_some());
            debug_assert!(self.session_parameters != vk::VideoSessionParametersKHR::null());
            let result = self.update_parameters_object(pps_set);
            debug_assert!(result == vk::Result::SUCCESS);
            result
        }
    }

    /// Flush all queued picture parameter sets, creating/updating the session
    /// parameters object for each.
    ///
    /// Mirrors `FlushPictureParametersQueue`.
    pub fn flush_picture_parameters_queue(
        &mut self,
        video_session: vk::VideoSessionKHR,
    ) -> i32 {
        if video_session == vk::VideoSessionKHR::null() {
            return -1;
        }

        let mut num_queue_items: u32 = 0;
        while let Some(pps_set) = self.picture_parameters_queue.pop_front() {
            let result = self.handle_new_picture_parameters_set(video_session, pps_set.as_ref());
            if result != vk::Result::SUCCESS {
                return -1;
            }
            num_queue_items += 1;
        }

        num_queue_items as i32
    }

    // ------------------------------------------------------------------
    // Hierarchy management
    // ------------------------------------------------------------------

    /// Build parent-child links between VPS, SPS, and PPS parameter sets in
    /// the queue. Returns `true` on success.
    ///
    /// Mirrors `UpdatePictureParametersHierarchy`.
    ///
    /// NOTE: The C++ version mutates `m_parent` on the `StdVideoPictureParametersSet`
    /// objects. In Rust this would require interior mutability on those objects.
    /// This port preserves the logic flow but parent assignment is a no-op
    /// placeholder until the trait supports it.
    pub fn update_picture_parameters_hierarchy(
        &mut self,
        pps_obj: &Arc<dyn StdVideoPictureParametersSetIf>,
    ) -> bool {
        let param_type = pps_obj.get_parameter_type();

        match param_type {
            ParameterType::PpsType => {
                let (node_id, is_node_id) = pps_obj.get_pps_id();
                if (node_id as u32) as usize >= MAX_PPS_IDS {
                    debug_assert!(false, "PPS ID is out of bounds");
                    return false;
                }
                debug_assert!(is_node_id);
                // Parent linkage: SPS -> PPS (placeholder).
            }
            ParameterType::SpsType => {
                let (node_id, is_node_id) = pps_obj.get_sps_id();
                if (node_id as u32) as usize >= MAX_SPS_IDS {
                    debug_assert!(false, "SPS ID is out of bounds");
                    return false;
                }
                debug_assert!(is_node_id);
                // Parent linkage: VPS -> SPS, child linkage: SPS -> PPS (placeholder).
            }
            ParameterType::VpsType => {
                let (node_id, is_node_id) = pps_obj.get_vps_id();
                if (node_id as u32) as usize >= MAX_VPS_IDS {
                    debug_assert!(false, "VPS ID is out of bounds");
                    return false;
                }
                debug_assert!(is_node_id);
                // Child linkage: VPS -> SPS (placeholder).
            }
            _ => {
                debug_assert!(false, "Invalid STD type");
                return false;
            }
        }

        // Track the last parameter set of each type.
        let idx = match param_type {
            ParameterType::PpsType => 0,
            ParameterType::SpsType => 1,
            ParameterType::VpsType => 2,
            ParameterType::Av1SpsType => 3,
            _ => return false,
        };
        self.last_pict_params_queue[idx] = Some(pps_obj.clone());

        true
    }

    // ------------------------------------------------------------------
    // Check / Add (static-like helpers)
    // ------------------------------------------------------------------

    /// Check whether a new Vulkan Picture Parameters object should be created
    /// or the existing one can be updated.
    ///
    /// Returns `true` if a new object must be created.
    ///
    /// Mirrors `CheckStdObjectBeforeUpdate`.
    pub fn check_std_object_before_update(
        std_pps: &dyn StdVideoPictureParametersSetIf,
        current: Option<&VkParserVideoPictureParameters>,
    ) -> bool {
        let std_object_update = std_pps.get_update_sequence_count() > 0;
        if current.is_none() || std_object_update {
            return true;
        }
        // Existing object — update path.
        false
    }

    /// Top-level entry point: add a picture parameter set, creating a new
    /// session parameters object if necessary.
    ///
    /// Mirrors the static `AddPictureParameters`.
    pub fn add_picture_parameters(
        vk_dev_ctx: *const (),
        video_session: vk::VideoSessionKHR,
        std_pps: Arc<dyn StdVideoPictureParametersSetIf>,
        current: &mut Option<Arc<VkParserVideoPictureParameters>>,
    ) -> vk::Result {
        // Flush the current object's queue first.
        if let Some(ref mut _cur) = current {
            // NOTE: requires interior mutability in production code (Arc<Mutex<...>>).
            // Omitted here to keep the port faithful to C++ structure.
            let _ = video_session; // would call cur.flush_picture_parameters_queue(video_session)
        }

        let need_new = Self::check_std_object_before_update(
            std_pps.as_ref(),
            current.as_deref(),
        );

        if need_new {
            let new_obj = Self::create(vk_dev_ctx, current.clone());
            match new_obj {
                Ok(obj) => *current = Some(obj),
                Err(e) => return e,
            }
        }

        // TODO: In production, call handle_new or add_to_queue on `current`.
        vk::Result::SUCCESS
    }

    // ------------------------------------------------------------------
    // Reset / Drop
    // ------------------------------------------------------------------

    /// Release all held resources and destroy the session parameters object.
    ///
    /// Mirrors `Reset`.
    pub fn reset(&mut self) {
        for sp in self.all_registered_params.drain(..) {
            sp.release_client_object();
        }
        while let Some(front) = self.picture_parameters_queue.pop_front() {
            front.release_client_object();
        }
        for slot in self.last_pict_params_queue.iter_mut() {
            if let Some(sp) = slot.take() {
                sp.release_client_object();
            }
        }
        // Reset template (recursive).
        // In production this would be done through Arc<Mutex<...>>.
        self.template_picture_parameters = None;

        if self.session_parameters != vk::VideoSessionParametersKHR::null() {
            // TODO: vkDestroyVideoSessionParametersKHR(...)
            self.session_parameters = vk::VideoSessionParametersKHR::null();
        }
        self.video_session = None;
    }

    /// Find a registered parameter set by pointer identity.
    ///
    /// Mirrors `FindByRawPtr`. Uses `Arc::ptr_eq` instead of raw pointer
    /// comparison.
    pub fn find_by_arc(
        &self,
        needle: &Arc<dyn StdVideoPictureParametersSetIf>,
    ) -> Option<Arc<dyn StdVideoPictureParametersSetIf>> {
        for sp in &self.all_registered_params {
            if Arc::ptr_eq(sp, needle) {
                return Some(sp.clone());
            }
        }
        None
    }
}

impl Drop for VkParserVideoPictureParameters {
    fn drop(&mut self) {
        self.reset();
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal mock implementing StdVideoPictureParametersSetIf for testing.
    struct MockPps {
        std_type: StdType,
        param_type: ParameterType,
        pps_id: i32,
        sps_id: i32,
        vps_id: i32,
        update_seq: u32,
    }

    impl MockPps {
        fn h264_sps(sps_id: i32) -> Self {
            Self {
                std_type: StdType::H264Sps,
                param_type: ParameterType::SpsType,
                pps_id: -1,
                sps_id,
                vps_id: -1,
                update_seq: 0,
            }
        }

        fn h264_pps(pps_id: i32, sps_id: i32) -> Self {
            Self {
                std_type: StdType::H264Pps,
                param_type: ParameterType::PpsType,
                pps_id,
                sps_id,
                vps_id: -1,
                update_seq: 0,
            }
        }

        fn h265_vps(vps_id: i32) -> Self {
            Self {
                std_type: StdType::H265Vps,
                param_type: ParameterType::VpsType,
                pps_id: -1,
                sps_id: -1,
                vps_id,
                update_seq: 0,
            }
        }

        fn av1_sps() -> Self {
            Self {
                std_type: StdType::Av1Sps,
                param_type: ParameterType::Av1SpsType,
                pps_id: -1,
                sps_id: -1,
                vps_id: -1,
                update_seq: 0,
            }
        }
    }

    impl StdVideoPictureParametersSetIf for MockPps {
        fn get_std_type(&self) -> StdType {
            self.std_type
        }
        fn get_parameter_type(&self) -> ParameterType {
            self.param_type
        }
        fn get_update_sequence_count(&self) -> u32 {
            self.update_seq
        }
        fn get_vps_id(&self) -> (i32, bool) {
            (self.vps_id, self.param_type == ParameterType::VpsType)
        }
        fn get_sps_id(&self) -> (i32, bool) {
            (self.sps_id, self.param_type == ParameterType::SpsType)
        }
        fn get_pps_id(&self) -> (i32, bool) {
            (self.pps_id, self.param_type == ParameterType::PpsType)
        }
        fn release_client_object(&self) {}
        fn get_client_object(&self) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
            None
        }
    }

    #[test]
    fn test_create_returns_valid_object() {
        let obj = VkParserVideoPictureParameters::create(std::ptr::null(), None);
        assert!(obj.is_ok());
        let obj = obj.unwrap();
        assert_eq!(obj.class_id, REF_CLASS_ID);
        assert_eq!(obj.id, -1); // Not yet assigned
    }

    #[test]
    fn test_bitset_vps() {
        let mut params = VkParserVideoPictureParameters::new(std::ptr::null(), None);
        assert!(!params.has_vps_id(0));
        params.vps_ids_used.set(0, true);
        assert!(params.has_vps_id(0));
        assert!(!params.has_vps_id(1));
        params.vps_ids_used.set(15, true);
        assert!(params.has_vps_id(15));
    }

    #[test]
    fn test_bitset_sps() {
        let mut params = VkParserVideoPictureParameters::new(std::ptr::null(), None);
        for i in 0..MAX_SPS_IDS {
            assert!(!params.has_sps_id(i as u32));
        }
        params.sps_ids_used.set(7, true);
        assert!(params.has_sps_id(7));
    }

    #[test]
    fn test_bitset_pps() {
        let mut params = VkParserVideoPictureParameters::new(std::ptr::null(), None);
        params.pps_ids_used.set(255, true);
        assert!(params.has_pps_id(255));
        assert!(!params.has_pps_id(0));
    }

    #[test]
    fn test_populate_h264_sps() {
        let sps = MockPps::h264_sps(5);
        let id = VkParserVideoPictureParameters::populate_h264_update_fields(&sps);
        assert_eq!(id, 5);
    }

    #[test]
    fn test_populate_h264_pps() {
        let pps = MockPps::h264_pps(42, 5);
        let id = VkParserVideoPictureParameters::populate_h264_update_fields(&pps);
        assert_eq!(id, 42);
    }

    #[test]
    fn test_populate_h265_vps() {
        let vps = MockPps::h265_vps(3);
        let id = VkParserVideoPictureParameters::populate_h265_update_fields(&vps);
        assert_eq!(id, 3);
    }

    #[test]
    fn test_create_parameters_object_assigns_id() {
        let mut params = VkParserVideoPictureParameters::new(std::ptr::null(), None);
        let sps = MockPps::h264_sps(0);
        let result = params.create_parameters_object(
            std::ptr::null(),
            vk::VideoSessionKHR::null(),
            &sps,
            None,
        );
        assert_eq!(result, vk::Result::SUCCESS);
        assert!(params.id > 0);
        assert!(params.has_sps_id(0));
    }

    #[test]
    fn test_update_parameters_object_tracks_ids() {
        let mut params = VkParserVideoPictureParameters::new(std::ptr::null(), None);
        // First create with SPS 0.
        let sps = MockPps::h264_sps(0);
        params.create_parameters_object(std::ptr::null(), vk::VideoSessionKHR::null(), &sps, None);
        params.session_parameters = {
            use vulkanalia::vk::Handle;
            unsafe { vk::VideoSessionParametersKHR::from_raw(1) }
        };

        // Now update with PPS 10.
        let pps = MockPps::h264_pps(10, 0);
        let result = params.update_parameters_object(&pps);
        assert_eq!(result, vk::Result::SUCCESS);
        assert!(params.has_pps_id(10));
        assert_eq!(params.update_count, 1);
    }

    #[test]
    fn test_add_to_queue() {
        let mut params = VkParserVideoPictureParameters::new(std::ptr::null(), None);
        let sps: Arc<dyn StdVideoPictureParametersSetIf> = Arc::new(MockPps::h264_sps(0));
        let result = params.add_picture_parameters_to_queue(sps);
        assert_eq!(result, vk::Result::SUCCESS);
        assert_eq!(params.picture_parameters_queue.len(), 1);
        assert_eq!(params.all_registered_params.len(), 1);
    }

    #[test]
    fn test_check_std_object_before_update_no_current() {
        let sps = MockPps::h264_sps(0);
        assert!(VkParserVideoPictureParameters::check_std_object_before_update(
            &sps, None
        ));
    }

    #[test]
    fn test_check_std_object_before_update_with_current_no_update() {
        let sps = MockPps::h264_sps(0);
        let params = VkParserVideoPictureParameters::new(std::ptr::null(), None);
        assert!(!VkParserVideoPictureParameters::check_std_object_before_update(
            &sps,
            Some(&params),
        ));
    }

    #[test]
    fn test_check_std_object_before_update_with_sequence_update() {
        let sps = MockPps {
            update_seq: 1,
            ..MockPps::h264_sps(0)
        };
        let params = VkParserVideoPictureParameters::new(std::ptr::null(), None);
        assert!(VkParserVideoPictureParameters::check_std_object_before_update(
            &sps,
            Some(&params),
        ));
    }

    #[test]
    fn test_reset_clears_state() {
        let mut params = VkParserVideoPictureParameters::new(std::ptr::null(), None);
        let sps: Arc<dyn StdVideoPictureParametersSetIf> = Arc::new(MockPps::h264_sps(0));
        params.add_picture_parameters_to_queue(sps);
        params.reset();
        assert!(params.picture_parameters_queue.is_empty());
        assert!(params.all_registered_params.is_empty());
        assert!(params.video_session.is_none());
    }

    #[test]
    fn test_av1_create_parameters() {
        let mut params = VkParserVideoPictureParameters::new(std::ptr::null(), None);
        let av1 = MockPps::av1_sps();
        let result = params.create_parameters_object(
            std::ptr::null(),
            vk::VideoSessionKHR::null(),
            &av1,
            None,
        );
        assert_eq!(result, vk::Result::SUCCESS);
        assert!(params.has_av1_pps_id(0));
    }

    #[test]
    fn test_from_base_valid() {
        let params = VkParserVideoPictureParameters::new(std::ptr::null(), None);
        assert!(VkParserVideoPictureParameters::from_base(&params).is_some());
    }

    #[test]
    fn test_flush_empty_queue() {
        let mut params = VkParserVideoPictureParameters::new(std::ptr::null(), None);
        // Null session should return -1.
        let result = params.flush_picture_parameters_queue(vk::VideoSessionKHR::null());
        assert_eq!(result, -1);
    }
}

// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Texture pools, slot rings, output pools and one-off acquisitions, and the claim tables they
//! share with interpreters.

pub(crate) mod gpu_pool_claim_table;
pub(crate) mod lease_aware_pool_slot_ring;
pub(super) mod node_output_pixel_buffer_pool_of_one_native_node;
pub(crate) mod node_output_surface_pool;
pub(super) mod one_off_acquisitions_counted_for_one_native_node;
pub(crate) mod one_off_acquisitions_held_for_one_node;
pub(crate) mod shared_pool_entry_freeing;
pub(crate) mod slots_outliving_their_producers;
pub mod texture_pool;
pub(crate) mod texture_registration;
pub(super) mod texture_ring;

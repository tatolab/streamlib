// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The native half of `streamlib-moq` — the module the wheel's two
//! `@processor` classes import as `streamlib_moq._native`.
//!
//! The engine never calls anything here. A processor extension's per-frame work
//! is its own package's Rust, reached directly from its own Python.

use pyo3::prelude::*;
use pyo3::types::PyBytes;
use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

pub mod annex_b_access_unit;
pub mod cmaf_fragment;
pub mod cmaf_init_segment;
pub mod cmaf_sample_entry;
pub mod cmaf_track_timeline;
pub mod encoded_media_sample;
pub mod error;
pub mod monotonic_clock;
pub mod moq_broadcast_catalog;
pub mod moq_relay_config;
pub mod moq_session;
pub mod streamlib_bag_object;
pub mod transport_stack;

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Throwaway spike for #1702: does an in-process Python processor (CPython
//! embedded via PyO3) beat today's subprocess-per-Python-processor model?
//!
//! Nothing here is an API proposal. The callback-injection token registry, the
//! process-global measurement collection point, and the Python-facing shapes are
//! all deliberate spike artifacts — #1702 defers API design until after the
//! numbers exist. The engine is untouched; every processor enters through the
//! existing `App::add_local` path.
//!
//! Tier A measures `source_emit_to_sink_receive`, NOT capture-to-present: there
//! is no camera and no display on this arm, and present time is not observable
//! in either arm without an engine change the spike forbids.

// Every fallible path returns the engine's own `streamlib::sdk::error::Error`,
// which is 168 bytes. Boxing it here would diverge the spike's signatures from
// the engine API it exists to measure.
#![allow(clippy::result_large_err)]

pub mod latency_measurement_recorder;
pub mod machine_specification_probe;
pub mod measuring_sink_processor;
pub mod monotonic_clock;
pub mod python_callback_stage_processor;
pub mod python_gil_attachment_anchor;
pub mod python_processor_callback_registry;
pub mod rust_passthrough_floor_stage_processor;
pub mod synthetic_frame_measurement_preamble;
pub mod synthetic_frame_source_processor;
pub mod synthetic_frame_wire_payload_mode;
pub mod tier_a_measurement_cell;
pub mod zero_copy_numpy_frame_view;

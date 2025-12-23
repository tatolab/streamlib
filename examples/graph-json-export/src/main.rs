// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Example: Graph JSON Export
//!
//! Demonstrates creating a graph with mock processors, connections, and components,
//! then exporting it to a JSON file with a timestamped filename.
//!
//! This example works only with the Graph data structure, not the runtime.

use std::fs::File;
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use streamlib::core::frames::DataFrame;
use streamlib::core::graph::{
    Graph, GraphEdgeWithComponents, GraphNodeWithComponents, InputLinkPortRef, OutputLinkPortRef,
};
use streamlib::core::links::{LinkInput, LinkOutput};
use streamlib::core::processors::ProcessorState;
use streamlib::core::JsonSerializableComponent;
use streamlib::Result;

// =============================================================================
// Mock Processor Configuration
// =============================================================================

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MockConfig {
    pub label: String,
}

// =============================================================================
// Mock Processors
// =============================================================================

/// Source processor with output ports only.
#[streamlib::processor(execution = Manual, description = "Generates data for downstream processors")]
struct SourceProcessor {
    #[streamlib::output(description = "Video output")]
    video_out: LinkOutput<DataFrame>,

    #[streamlib::output(description = "Audio output")]
    audio_out: LinkOutput<DataFrame>,

    #[streamlib::config]
    config: MockConfig,
}

impl streamlib::ManualProcessor for SourceProcessor::Processor {
    fn process(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Transform processor with both input and output ports.
#[streamlib::processor(execution = Manual, description = "Transforms data from input to output")]
struct TransformProcessor {
    #[streamlib::input(description = "Data input")]
    input: LinkInput<DataFrame>,

    #[streamlib::output(description = "Transformed output")]
    output: LinkOutput<DataFrame>,

    #[streamlib::config]
    config: MockConfig,
}

impl streamlib::ManualProcessor for TransformProcessor::Processor {
    fn process(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Sink processor with input ports only.
#[streamlib::processor(execution = Manual, description = "Consumes data from upstream processors")]
struct SinkProcessor {
    #[streamlib::input(description = "Video input")]
    video_in: LinkInput<DataFrame>,

    #[streamlib::input(description = "Audio input")]
    audio_in: LinkInput<DataFrame>,

    #[streamlib::config]
    config: MockConfig,
}

impl streamlib::ManualProcessor for SinkProcessor::Processor {
    fn process(&mut self) -> Result<()> {
        Ok(())
    }
}

// =============================================================================
// Mock Components
// =============================================================================

/// State component for processors.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MockStateComponent(ProcessorState);

impl Default for MockStateComponent {
    fn default() -> Self {
        Self(ProcessorState::Idle)
    }
}

impl JsonSerializableComponent for MockStateComponent {
    fn json_key(&self) -> &'static str {
        "mock_state"
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({ "state": format!("{:?}", self.0) })
    }
}

/// Metrics component for processors.
#[derive(Debug, Clone)]
struct MockMetricsComponent {
    frames_processed: u64,
    average_latency_ms: f64,
}

impl MockMetricsComponent {
    fn new(frames_processed: u64, average_latency_ms: f64) -> Self {
        Self {
            frames_processed,
            average_latency_ms,
        }
    }
}

impl JsonSerializableComponent for MockMetricsComponent {
    fn json_key(&self) -> &'static str {
        "mock_metrics"
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "frames_processed": self.frames_processed,
            "average_latency_ms": self.average_latency_ms
        })
    }
}

/// Link statistics component demonstrating selective field serialization.
///
/// Some fields are included in JSON output, others are intentionally hidden
/// (e.g., internal implementation details, raw pointers, or debug-only data).
#[derive(Debug, Clone)]
struct MockLinkStatsComponent {
    /// Included in JSON: number of frames transferred
    frames_transferred: u64,
    /// Included in JSON: current buffer utilization (0.0 - 1.0)
    buffer_utilization: f64,
    /// HIDDEN from JSON: internal sequence number for debugging
    #[allow(dead_code)]
    internal_sequence_number: u64,
    /// HIDDEN from JSON: raw memory address (would be meaningless in JSON)
    #[allow(dead_code)]
    hidden_debug_pointer: usize,
    /// HIDDEN from JSON: temporary calculation cache
    #[allow(dead_code)]
    cached_throughput_calculation: f64,
}

impl MockLinkStatsComponent {
    fn new(frames_transferred: u64, buffer_utilization: f64) -> Self {
        Self {
            frames_transferred,
            buffer_utilization,
            // These are internal/debug fields not exposed via JSON
            internal_sequence_number: 42,
            hidden_debug_pointer: 0xDEADBEEF,
            cached_throughput_calculation: frames_transferred as f64 * 0.033,
        }
    }
}

impl JsonSerializableComponent for MockLinkStatsComponent {
    fn json_key(&self) -> &'static str {
        "link_stats"
    }

    fn to_json(&self) -> serde_json::Value {
        // Only expose the public, meaningful fields
        // Hidden fields: internal_sequence_number, hidden_debug_pointer, cached_throughput_calculation
        serde_json::json!({
            "frames_transferred": self.frames_transferred,
            "buffer_utilization": self.buffer_utilization
        })
    }
}

// =============================================================================
// Main
// =============================================================================

fn main() {
    println!("Graph JSON Export Example");
    println!("=========================\n");

    // Create a new graph
    let mut graph = Graph::new();

    // -------------------------------------------------------------------------
    // Add processors
    // -------------------------------------------------------------------------
    println!("Adding processors...");

    let source_id = graph
        .traversal_mut()
        .add_v(SourceProcessor::Processor::node(MockConfig {
            label: "Camera Source".to_string(),
        }))
        .first()
        .expect("should create source processor")
        .id
        .to_string();
    println!("  Created SourceProcessor: {}", source_id);

    let video_transform_id = graph
        .traversal_mut()
        .add_v(TransformProcessor::Processor::node(MockConfig {
            label: "Video Encoder".to_string(),
        }))
        .first()
        .expect("should create video transform processor")
        .id
        .to_string();
    println!(
        "  Created TransformProcessor (video): {}",
        video_transform_id
    );

    let audio_transform_id = graph
        .traversal_mut()
        .add_v(TransformProcessor::Processor::node(MockConfig {
            label: "Audio Encoder".to_string(),
        }))
        .first()
        .expect("should create audio transform processor")
        .id
        .to_string();
    println!(
        "  Created TransformProcessor (audio): {}",
        audio_transform_id
    );

    let sink_id = graph
        .traversal_mut()
        .add_v(SinkProcessor::Processor::node(MockConfig {
            label: "File Writer".to_string(),
        }))
        .first()
        .expect("should create sink processor")
        .id
        .to_string();
    println!("  Created SinkProcessor: {}", sink_id);

    // -------------------------------------------------------------------------
    // Add connections (links)
    // -------------------------------------------------------------------------
    println!("\nAdding connections...");

    // Source video_out -> Video Transform input
    let link1_id = graph
        .traversal_mut()
        .add_e(
            OutputLinkPortRef::new(&source_id, "video_out"),
            InputLinkPortRef::new(&video_transform_id, "input"),
        )
        .first()
        .expect("should create link")
        .id
        .to_string();
    println!(
        "  Link: {} (video_out) -> {} (input)",
        source_id, video_transform_id
    );

    // Source audio_out -> Audio Transform input
    let link2_id = graph
        .traversal_mut()
        .add_e(
            OutputLinkPortRef::new(&source_id, "audio_out"),
            InputLinkPortRef::new(&audio_transform_id, "input"),
        )
        .first()
        .expect("should create link")
        .id
        .to_string();
    println!(
        "  Link: {} (audio_out) -> {} (input)",
        source_id, audio_transform_id
    );

    // Video Transform output -> Sink video_in
    let link3_id = graph
        .traversal_mut()
        .add_e(
            OutputLinkPortRef::new(&video_transform_id, "output"),
            InputLinkPortRef::new(&sink_id, "video_in"),
        )
        .first()
        .expect("should create link")
        .id
        .to_string();
    println!(
        "  Link: {} (output) -> {} (video_in)",
        video_transform_id, sink_id
    );

    // Audio Transform output -> Sink audio_in
    let link4_id = graph
        .traversal_mut()
        .add_e(
            OutputLinkPortRef::new(&audio_transform_id, "output"),
            InputLinkPortRef::new(&sink_id, "audio_in"),
        )
        .first()
        .expect("should create link")
        .id
        .to_string();
    println!(
        "  Link: {} (output) -> {} (audio_in)",
        audio_transform_id, sink_id
    );

    // -------------------------------------------------------------------------
    // Add components to processors
    // -------------------------------------------------------------------------
    println!("\nAdding components to processors...");

    // Add state components
    graph
        .traversal_mut()
        .v(source_id.as_str())
        .first_mut()
        .unwrap()
        .insert(MockStateComponent(ProcessorState::Running));
    println!("  {} state: Running", source_id);

    graph
        .traversal_mut()
        .v(video_transform_id.as_str())
        .first_mut()
        .unwrap()
        .insert(MockStateComponent(ProcessorState::Running));
    println!("  {} state: Running", video_transform_id);

    graph
        .traversal_mut()
        .v(audio_transform_id.as_str())
        .first_mut()
        .unwrap()
        .insert(MockStateComponent(ProcessorState::Running));
    println!("  {} state: Running", audio_transform_id);

    graph
        .traversal_mut()
        .v(sink_id.as_str())
        .first_mut()
        .unwrap()
        .insert(MockStateComponent(ProcessorState::Running));
    println!("  {} state: Running", sink_id);

    // Add metrics components
    graph
        .traversal_mut()
        .v(source_id.as_str())
        .first_mut()
        .unwrap()
        .insert(MockMetricsComponent::new(1000, 2.5));
    println!("  {} metrics: 1000 frames, 2.5ms avg latency", source_id);

    graph
        .traversal_mut()
        .v(video_transform_id.as_str())
        .first_mut()
        .unwrap()
        .insert(MockMetricsComponent::new(1000, 8.3));
    println!(
        "  {} metrics: 1000 frames, 8.3ms avg latency",
        video_transform_id
    );

    // -------------------------------------------------------------------------
    // Add components to links
    // -------------------------------------------------------------------------
    println!("\nAdding components to links...");

    // Link 1: Source -> Video Transform (high traffic video link)
    graph
        .traversal_mut()
        .e(link1_id.as_str())
        .first_mut()
        .unwrap()
        .insert(MockLinkStatsComponent::new(15000, 0.75));
    println!("  {} stats: 15000 frames, 75% buffer utilization", link1_id);

    // Link 2: Source -> Audio Transform (lower traffic audio link)
    graph
        .traversal_mut()
        .e(link2_id.as_str())
        .first_mut()
        .unwrap()
        .insert(MockLinkStatsComponent::new(48000, 0.30));
    println!("  {} stats: 48000 frames, 30% buffer utilization", link2_id);

    // Link 3: Video Transform -> Sink
    graph
        .traversal_mut()
        .e(link3_id.as_str())
        .first_mut()
        .unwrap()
        .insert(MockLinkStatsComponent::new(14500, 0.65));
    println!("  {} stats: 14500 frames, 65% buffer utilization", link3_id);

    // Link 4: Audio Transform -> Sink
    graph
        .traversal_mut()
        .e(link4_id.as_str())
        .first_mut()
        .unwrap()
        .insert(MockLinkStatsComponent::new(47500, 0.25));
    println!("  {} stats: 47500 frames, 25% buffer utilization", link4_id);

    // -------------------------------------------------------------------------
    // Print graph summary
    // -------------------------------------------------------------------------
    println!("\nGraph Summary:");
    println!("  Processors: {}", graph.traversal().v(()).ids().len());
    println!("  Links: {}", graph.traversal().e(()).ids().len());

    // -------------------------------------------------------------------------
    // Serialize to JSON
    // -------------------------------------------------------------------------
    println!("\nSerializing graph to JSON...");

    let json = serde_json::to_string_pretty(&graph).expect("should serialize graph");

    // Generate timestamped filename in output folder
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time went backwards")
        .as_secs();

    // Get the directory where the example lives
    let output_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("output");
    let filename = output_dir.join(format!("graph_export_{}.json", timestamp));

    // Write to file
    let mut file = File::create(&filename).expect("should create file");
    file.write_all(json.as_bytes()).expect("should write file");

    println!("  Exported to: {}", filename.display());
    println!("\nDone!");

    // Suppress unused variable warnings
    let _ = (link1_id, link2_id, link3_id, link4_id);
}

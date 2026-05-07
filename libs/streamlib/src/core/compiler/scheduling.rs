// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Processor scheduling logic for the compiler.

use crate::core::execution::ThreadPriority;
use crate::core::graph::ProcessorNode;

/// How a processor should be scheduled at runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchedulingStrategy {
    /// Dedicated OS thread with configurable priority.
    DedicatedThread {
        priority: ThreadPriority,
        name: Option<String>,
    },
}

impl Default for SchedulingStrategy {
    fn default() -> Self {
        SchedulingStrategy::DedicatedThread {
            priority: ThreadPriority::Normal,
            name: None,
        }
    }
}

impl SchedulingStrategy {
    /// Get a human-readable description.
    pub fn description(&self) -> String {
        match self {
            SchedulingStrategy::DedicatedThread { priority, name } => {
                if let Some(n) = name {
                    format!("dedicated thread '{}' ({})", n, priority.description())
                } else {
                    format!("dedicated thread ({})", priority.description())
                }
            }
        }
    }
}

/// Determine scheduling strategy for a processor based on its type.
///
/// The PascalCase short-name segment of the structured ident drives the
/// heuristic — package/org are intentionally ignored so any package
/// shipping a processor whose short name contains "Audio", "Camera",
/// etc. picks up the right thread priority.
pub(crate) fn scheduling_strategy_for_processor(node: &ProcessorNode) -> SchedulingStrategy {
    let type_name = node.processor_type.r#type.as_str();

    // Camera processors - dedicated thread with high priority
    if type_name == "CameraProcessor" || type_name.contains("Camera") {
        return SchedulingStrategy::DedicatedThread {
            priority: ThreadPriority::High,
            name: Some(format!("camera-{}", node.id)),
        };
    }

    // Display processors - dedicated thread with high priority
    if type_name == "DisplayProcessor" || type_name.contains("Display") {
        return SchedulingStrategy::DedicatedThread {
            priority: ThreadPriority::High,
            name: Some(format!("display-{}", node.id)),
        };
    }

    // Audio processors get real-time priority
    if type_name.contains("Audio")
        || type_name.contains("Microphone")
        || type_name.contains("Speaker")
    {
        return SchedulingStrategy::DedicatedThread {
            priority: ThreadPriority::RealTime,
            name: Some(format!("audio-{}", node.id)),
        };
    }

    // Video encoding/decoding gets high priority
    if type_name.contains("Encoder")
        || type_name.contains("Decoder")
        || type_name.contains("H264")
        || type_name.contains("H265")
    {
        return SchedulingStrategy::DedicatedThread {
            priority: ThreadPriority::High,
            name: Some(format!("video-{}", node.id)),
        };
    }

    // Compositors get real-time priority (video processing with strict timing)
    if type_name.contains("Compositor") {
        return SchedulingStrategy::DedicatedThread {
            priority: ThreadPriority::RealTime,
            name: Some(format!("compositor-{}", node.id)),
        };
    }

    // Default: normal dedicated thread
    SchedulingStrategy::DedicatedThread {
        priority: ThreadPriority::Normal,
        name: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::descriptors::{Org, Package, SchemaIdent, SemVer, TypeName};

    fn ident(short_name: &str) -> SchemaIdent {
        SchemaIdent::new(
            Org::new("tatolab").unwrap(),
            Package::new("streamlib").unwrap(),
            TypeName::new(short_name).unwrap(),
            SemVer::new(1, 0, 0),
        )
    }

    #[test]
    fn test_scheduling_camera() {
        let node = ProcessorNode::new(
            ident("CameraProcessor"),
            "CameraProcessor",
            None,
            vec![],
            vec![],
        );
        match scheduling_strategy_for_processor(&node) {
            SchedulingStrategy::DedicatedThread { priority, .. } => {
                assert_eq!(priority, ThreadPriority::High);
            }
        }
    }

    #[test]
    fn test_scheduling_audio() {
        let node = ProcessorNode::new(
            ident("AudioCaptureProcessor"),
            "AudioCaptureProcessor",
            None,
            vec![],
            vec![],
        );
        match scheduling_strategy_for_processor(&node) {
            SchedulingStrategy::DedicatedThread { priority, .. } => {
                assert_eq!(priority, ThreadPriority::RealTime);
            }
        }
    }

    #[test]
    fn test_scheduling_encoder() {
        let node = ProcessorNode::new(
            ident("H264Encoder"),
            "H264Encoder",
            None,
            vec![],
            vec![],
        );
        match scheduling_strategy_for_processor(&node) {
            SchedulingStrategy::DedicatedThread { priority, .. } => {
                assert_eq!(priority, ThreadPriority::High);
            }
        }
    }

    #[test]
    fn test_scheduling_generic() {
        let node = ProcessorNode::new(
            ident("SomeProcessor"),
            "SomeProcessor",
            None,
            vec![],
            vec![],
        );
        match scheduling_strategy_for_processor(&node) {
            SchedulingStrategy::DedicatedThread { priority, .. } => {
                assert_eq!(priority, ThreadPriority::Normal);
            }
        }
    }
}

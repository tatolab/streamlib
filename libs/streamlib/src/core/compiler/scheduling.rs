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
pub(crate) fn scheduling_strategy_for_processor(node: &ProcessorNode) -> SchedulingStrategy {
    let processor_type = &node.processor_type;

    // Camera processors - dedicated thread with high priority
    if processor_type == "CameraProcessor" || processor_type.contains("Camera") {
        return SchedulingStrategy::DedicatedThread {
            priority: ThreadPriority::High,
            name: Some(format!("camera-{}", node.id)),
        };
    }

    // Display processors - dedicated thread with high priority
    if processor_type == "DisplayProcessor" || processor_type.contains("Display") {
        return SchedulingStrategy::DedicatedThread {
            priority: ThreadPriority::High,
            name: Some(format!("display-{}", node.id)),
        };
    }

    // Audio processors get real-time priority
    if processor_type.contains("Audio")
        || processor_type.contains("Microphone")
        || processor_type.contains("Speaker")
    {
        return SchedulingStrategy::DedicatedThread {
            priority: ThreadPriority::RealTime,
            name: Some(format!("audio-{}", node.id)),
        };
    }

    // Video encoding/decoding gets high priority
    if processor_type.contains("Encoder")
        || processor_type.contains("Decoder")
        || processor_type.contains("H264")
        || processor_type.contains("H265")
    {
        return SchedulingStrategy::DedicatedThread {
            priority: ThreadPriority::High,
            name: Some(format!("video-{}", node.id)),
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

    #[test]
    fn test_scheduling_camera() {
        let node = ProcessorNode::new("CameraProcessor", "CameraProcessor", None, vec![], vec![]);
        match scheduling_strategy_for_processor(&node) {
            SchedulingStrategy::DedicatedThread { priority, .. } => {
                assert_eq!(priority, ThreadPriority::High);
            }
        }
    }

    #[test]
    fn test_scheduling_audio() {
        let node = ProcessorNode::new(
            "AudioCaptureProcessor",
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
        let node = ProcessorNode::new("H264Encoder", "H264Encoder", None, vec![], vec![]);
        match scheduling_strategy_for_processor(&node) {
            SchedulingStrategy::DedicatedThread { priority, .. } => {
                assert_eq!(priority, ThreadPriority::High);
            }
        }
    }

    #[test]
    fn test_scheduling_generic() {
        let node = ProcessorNode::new("SomeProcessor", "SomeProcessor", None, vec![], vec![]);
        match scheduling_strategy_for_processor(&node) {
            SchedulingStrategy::DedicatedThread { priority, .. } => {
                assert_eq!(priority, ThreadPriority::Normal);
            }
        }
    }
}

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Default scheduler delegate implementation.

use crate::core::delegates::{SchedulerDelegate, SchedulingStrategy, ThreadPriority};
use crate::core::graph::ProcessorNode;

/// Default scheduler implementation using type-based heuristics.
pub struct DefaultScheduler;

impl Default for DefaultScheduler {
    fn default() -> Self {
        Self
    }
}

impl SchedulerDelegate for DefaultScheduler {
    fn scheduling_strategy(&self, node: &ProcessorNode) -> SchedulingStrategy {
        let processor_type = &node.processor_type;

        // Camera processors - dedicated thread, dispatch to main for AVFoundation
        if processor_type == "CameraProcessor" || processor_type.contains("Camera") {
            return SchedulingStrategy::DedicatedThread {
                priority: ThreadPriority::High,
                name: Some(format!("camera-{}", node.id)),
            };
        }

        // Display processors - dedicated thread, dispatch to main for UI
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_scheduler_camera() {
        let scheduler = DefaultScheduler;
        let node = ProcessorNode::new("cam".into(), "CameraProcessor".into(), None, vec![], vec![]);

        match scheduler.scheduling_strategy(&node) {
            SchedulingStrategy::DedicatedThread { priority, .. } => {
                assert_eq!(priority, ThreadPriority::High);
            }
            _ => panic!("Expected DedicatedThread for camera processor"),
        }
    }

    #[test]
    fn test_default_scheduler_audio() {
        let scheduler = DefaultScheduler;
        let node = ProcessorNode::new(
            "mic".into(),
            "AudioCaptureProcessor".into(),
            None,
            vec![],
            vec![],
        );

        match scheduler.scheduling_strategy(&node) {
            SchedulingStrategy::DedicatedThread { priority, .. } => {
                assert_eq!(priority, ThreadPriority::RealTime);
            }
            _ => panic!("Expected DedicatedThread for audio processor"),
        }
    }

    #[test]
    fn test_default_scheduler_encoder() {
        let scheduler = DefaultScheduler;
        let node = ProcessorNode::new("enc".into(), "H264Encoder".into(), None, vec![], vec![]);

        match scheduler.scheduling_strategy(&node) {
            SchedulingStrategy::DedicatedThread { priority, .. } => {
                assert_eq!(priority, ThreadPriority::High);
            }
            _ => panic!("Expected DedicatedThread for encoder"),
        }
    }

    #[test]
    fn test_default_scheduler_generic() {
        let scheduler = DefaultScheduler;
        let node = ProcessorNode::new("proc".into(), "SomeProcessor".into(), None, vec![], vec![]);

        match scheduler.scheduling_strategy(&node) {
            SchedulingStrategy::DedicatedThread { priority, .. } => {
                assert_eq!(priority, ThreadPriority::Normal);
            }
            _ => panic!("Expected DedicatedThread for generic processor"),
        }
    }
}

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use super::metadata::MetadataValue;
use crate::core::links::{LinkPortMessage, LinkPortType};
use std::collections::HashMap;
use std::sync::Arc;

// Implement sealed trait
impl crate::core::links::traits::link_port_message::sealed::Sealed for VideoFrame {}

#[derive(Clone)]
pub struct VideoFrame {
    pub texture: Arc<wgpu::Texture>,

    pub format: wgpu::TextureFormat,

    pub timestamp_ns: i64,

    pub frame_number: u64,

    pub width: u32,

    pub height: u32,

    pub metadata: Option<HashMap<String, MetadataValue>>,
}

impl VideoFrame {
    pub fn new(
        texture: Arc<wgpu::Texture>,
        format: wgpu::TextureFormat,
        timestamp_ns: i64,
        frame_number: u64,
        width: u32,
        height: u32,
    ) -> Self {
        Self {
            texture,
            format,
            timestamp_ns,
            frame_number,
            width,
            height,
            metadata: None,
        }
    }

    pub fn with_metadata(
        texture: Arc<wgpu::Texture>,
        format: wgpu::TextureFormat,
        timestamp_ns: i64,
        frame_number: u64,
        width: u32,
        height: u32,
        metadata: HashMap<String, MetadataValue>,
    ) -> Self {
        Self {
            texture,
            format,
            timestamp_ns,
            frame_number,
            width,
            height,
            metadata: Some(metadata),
        }
    }

    pub fn example_720p() -> serde_json::Value {
        serde_json::json!({
            "width": 1280,
            "height": 720,
            "format": "Rgba8Unorm",
            "timestamp_ns": 33_000_000,
            "frame_number": 1,
            "metadata": {}
        })
    }

    pub fn example_1080p() -> serde_json::Value {
        serde_json::json!({
            "width": 1920,
            "height": 1080,
            "format": "Rgba8Unorm",
            "timestamp_ns": 33_000_000,
            "frame_number": 1,
            "metadata": {}
        })
    }

    pub fn example_4k() -> serde_json::Value {
        serde_json::json!({
            "width": 3840,
            "height": 2160,
            "format": "Rgba8Unorm",
            "timestamp_ns": 33_000_000,
            "frame_number": 1,
            "metadata": {}
        })
    }
}

impl LinkPortMessage for VideoFrame {
    fn port_type() -> LinkPortType {
        LinkPortType::Video
    }

    fn schema() -> std::sync::Arc<crate::core::Schema> {
        std::sync::Arc::clone(&crate::core::SCHEMA_VIDEO_FRAME)
    }

    fn examples() -> Vec<(&'static str, serde_json::Value)> {
        vec![
            ("720p video", Self::example_720p()),
            ("1080p video", Self::example_1080p()),
            ("4K video", Self::example_4k()),
        ]
    }
}

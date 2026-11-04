// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The protocol between a `BrowserSource` node and the machine's browser host.
//!
//! Each node opens one Unix socket connection to the host per tab. Every
//! message is a little-endian `u32` byte length, then that many bytes of one
//! msgpack-encoded message. A descriptor a message carries rides as
//! `SCM_RIGHTS` on the write that sends the message's first byte. The node's
//! first message is [`TabProtocolMessageFromNode::OpenTab`]; a host that speaks
//! another [`TAB_PROTOCOL_VERSION`] refuses it by name.

mod browser_host_tab_frame_slots;
mod tab_protocol_socket;

pub use browser_host_tab_frame_slots::{
    BrowserHostTabFrameSlots, FRAME_SLOT_BYTES_PER_PIXEL, frame_slot_byte_size,
};
pub use tab_protocol_socket::{
    MOST_TAB_PROTOCOL_MESSAGE_BYTES, TabProtocolMessageReader, TabProtocolMessageWriter,
    TabProtocolSocketFailure,
};

use serde::{Deserialize, Serialize};

/// The protocol version this build speaks; a peer speaking another is refused.
pub const TAB_PROTOCOL_VERSION: u32 = 1;

/// The shared service name a node leases the browser host by.
pub const BROWSER_HOST_SHARED_NATIVE_SERVICE_NAME: &str = "browser-host";

/// What a node asks the host for a page with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrowserHostTabRequest {
    /// The stream the node belongs to; its tabs share one CEF profile.
    pub tenant_stream_id: String,
    pub url: String,
    pub width: u32,
    pub height: u32,
    pub frame_rate: u32,
    pub transparent_background: bool,
    pub audio_sample_rate: u32,
    /// How many painted frames the host may hand the node before it is
    /// given one back.
    pub frame_slot_count: u32,
}

/// One message a node sends the host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TabProtocolMessageFromNode {
    /// The first message on a connection: the page to open.
    OpenTab {
        tab_protocol_version: u32,
        tab_request: BrowserHostTabRequest,
    },
    /// One `data` message's detail, as JSON text, for the page's
    /// `tatolab:data` listeners.
    DeliverPageData { detail_json: String },
    /// The node has published the frame in `slot` and may be handed it again.
    ReleaseFrameSlot { slot: u32 },
    /// Close the page; the host closes it as well when the connection ends.
    CloseTab,
}

/// How loud a line the host logs under a tab is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserHostTabLogLevel {
    Error,
    Warn,
    Info,
    Debug,
}

/// One message the host sends a node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TabProtocolMessageFromBrowserHost {
    /// The page is open. The tab's frame slots ride on this message as one
    /// descriptor, [`BrowserHostTabFrameSlots`] of the request's extent.
    TabOpened {
        tab_protocol_version: u32,
        browser_host_process_id: u32,
    },
    /// The host did not open the page, and why.
    TabRefused { reason: String },
    /// The page painted into `slot`: BGRA rows, top first.
    FramePainted {
        slot: u32,
        width: u32,
        height: u32,
        row_stride_bytes: u32,
    },
    /// The page opened an audio stream.
    AudioStreamStarted { sample_rate: u32, channels: u32 },
    /// One packet of the open stream: `sample_count` frames of interleaved
    /// little-endian `f32` samples.
    AudioPacket {
        sample_count: u32,
        #[serde(with = "serde_bytes_as_bin")]
        interleaved_samples_le_f32: Vec<u8>,
    },
    /// The page's audio stream ended.
    AudioStreamStopped,
    /// One `window.tatolab.publish(object)` call's object, as JSON text.
    PagePublished { published_json: String },
    /// A line the host logs about this tab, for the node's log.
    TabLog {
        level: BrowserHostTabLogLevel,
        message: String,
    },
    /// The host stopped rendering the page for good, and why; the node fails.
    PageRenderingGaveUp { reason: String },
}

impl TabProtocolMessageFromBrowserHost {
    /// How many descriptors ride on this message.
    pub fn attached_descriptor_count(&self) -> usize {
        match self {
            Self::TabOpened { .. } => 1,
            _ => 0,
        }
    }
}

/// Encode a byte vector as msgpack `bin`, never as an array of integers.
mod serde_bytes_as_bin {
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(bytes)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        struct ByteBufferVisitor;
        impl<'de> serde::de::Visitor<'de> for ByteBufferVisitor {
            type Value = Vec<u8>;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("msgpack bin")
            }
            fn visit_bytes<E: serde::de::Error>(self, bytes: &[u8]) -> Result<Vec<u8>, E> {
                Ok(bytes.to_vec())
            }
            fn visit_byte_buf<E: serde::de::Error>(self, bytes: Vec<u8>) -> Result<Vec<u8>, E> {
                Ok(bytes)
            }
        }
        deserializer.deserialize_byte_buf(ByteBufferVisitor)
    }
}

/// The most frame slots one tab may ask for.
pub const MOST_FRAME_SLOTS_PER_TAB: u32 = 8;

impl BrowserHostTabRequest {
    /// Refuse what CEF's windowless renderer cannot paint, before CEF is touched.
    pub fn refuse_what_cef_cannot_render(&self) -> Result<(), String> {
        if self.url.trim().is_empty() {
            return Err("a browser source needs a url".to_string());
        }
        if self.tenant_stream_id.is_empty() {
            return Err("a tab names the stream it belongs to".to_string());
        }
        if self.width == 0 || self.height == 0 || self.width > 8192 || self.height > 8192 {
            return Err(format!(
                "a page renders at 1x1 to 8192x8192, not {}x{}",
                self.width, self.height
            ));
        }
        if !(1..=60).contains(&self.frame_rate) {
            return Err(format!(
                "CEF paints a windowless page at 1 to 60 frames a second, not {}",
                self.frame_rate
            ));
        }
        if !(8_000..=192_000).contains(&self.audio_sample_rate) {
            return Err(format!(
                "audio_sample_rate is 8000 to 192000 Hz, not {}",
                self.audio_sample_rate
            ));
        }
        if !(1..=MOST_FRAME_SLOTS_PER_TAB).contains(&self.frame_slot_count) {
            return Err(format!(
                "a tab hands its node 1 to {MOST_FRAME_SLOTS_PER_TAB} frames at once, not {}",
                self.frame_slot_count
            ));
        }
        Ok(())
    }
}

/// The refusal a host gives an `OpenTab` of another protocol version.
pub fn refusal_of_another_tab_protocol_version(node_tab_protocol_version: u32) -> String {
    format!(
        "this browser host speaks tab protocol {TAB_PROTOCOL_VERSION} and the node speaks \
         {node_tab_protocol_version}; the runtime and tatolab-browser must come from matching \
         releases"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab_request() -> BrowserHostTabRequest {
        BrowserHostTabRequest {
            tenant_stream_id: "lower-third".to_string(),
            url: "https://example.com".to_string(),
            width: 1920,
            height: 1080,
            frame_rate: 60,
            transparent_background: true,
            audio_sample_rate: 48_000,
            frame_slot_count: 3,
        }
    }

    #[test]
    fn every_message_a_node_sends_survives_the_wire() {
        for message in [
            TabProtocolMessageFromNode::OpenTab {
                tab_protocol_version: TAB_PROTOCOL_VERSION,
                tab_request: tab_request(),
            },
            TabProtocolMessageFromNode::DeliverPageData {
                detail_json: "{\"title\":\"Ada\"}".to_string(),
            },
            TabProtocolMessageFromNode::ReleaseFrameSlot { slot: 2 },
            TabProtocolMessageFromNode::CloseTab,
        ] {
            let encoded = rmp_serde::to_vec_named(&message).unwrap();
            let decoded: TabProtocolMessageFromNode = rmp_serde::from_slice(&encoded).unwrap();
            assert_eq!(decoded, message);
        }
    }

    #[test]
    fn every_message_the_host_sends_survives_the_wire() {
        for message in [
            TabProtocolMessageFromBrowserHost::TabOpened {
                tab_protocol_version: TAB_PROTOCOL_VERSION,
                browser_host_process_id: 41,
            },
            TabProtocolMessageFromBrowserHost::TabRefused {
                reason: "no".to_string(),
            },
            TabProtocolMessageFromBrowserHost::FramePainted {
                slot: 1,
                width: 1920,
                height: 1080,
                row_stride_bytes: 7680,
            },
            TabProtocolMessageFromBrowserHost::AudioStreamStarted {
                sample_rate: 48_000,
                channels: 2,
            },
            TabProtocolMessageFromBrowserHost::AudioPacket {
                sample_count: 2,
                interleaved_samples_le_f32: [0.5f32, -0.5, 0.25, -0.25]
                    .iter()
                    .flat_map(|sample| sample.to_le_bytes())
                    .collect(),
            },
            TabProtocolMessageFromBrowserHost::AudioStreamStopped,
            TabProtocolMessageFromBrowserHost::PagePublished {
                published_json: "{\"clicked\":true}".to_string(),
            },
            TabProtocolMessageFromBrowserHost::TabLog {
                level: BrowserHostTabLogLevel::Warn,
                message: "renderer gone".to_string(),
            },
            TabProtocolMessageFromBrowserHost::PageRenderingGaveUp {
                reason: "the renderer ended 6 times".to_string(),
            },
        ] {
            let encoded = rmp_serde::to_vec_named(&message).unwrap();
            let decoded: TabProtocolMessageFromBrowserHost =
                rmp_serde::from_slice(&encoded).unwrap();
            assert_eq!(decoded, message);
        }
    }

    #[test]
    fn audio_samples_travel_as_one_bin_not_an_array_of_integers() {
        let message = TabProtocolMessageFromBrowserHost::AudioPacket {
            sample_count: 1,
            interleaved_samples_le_f32: vec![0xAB; 8],
        };
        let encoded = rmp_serde::to_vec_named(&message).unwrap();
        let bin8_header_then_length = [0xC4, 8];
        assert!(
            encoded
                .windows(bin8_header_then_length.len())
                .any(|window| window == bin8_header_then_length),
            "{encoded:02x?}"
        );
    }

    #[test]
    fn the_message_tag_is_the_wire_spelling() {
        let encoded = rmp_serde::to_vec_named(&TabProtocolMessageFromNode::CloseTab).unwrap();
        let as_value: rmpv::Value = rmpv::decode::read_value(&mut encoded.as_slice()).unwrap();
        assert_eq!(as_value["kind"].as_str(), Some("close_tab"));
    }

    #[test]
    fn only_tab_opened_carries_a_descriptor() {
        assert_eq!(
            TabProtocolMessageFromBrowserHost::TabOpened {
                tab_protocol_version: 1,
                browser_host_process_id: 1
            }
            .attached_descriptor_count(),
            1
        );
        assert_eq!(
            TabProtocolMessageFromBrowserHost::AudioStreamStopped.attached_descriptor_count(),
            0
        );
    }

    #[test]
    fn a_request_cef_can_render_passes() {
        tab_request()
            .refuse_what_cef_cannot_render()
            .expect("1080p60 renders");
    }

    #[test]
    fn what_cef_cannot_render_is_refused_by_name() {
        for (broken, named) in [
            (
                BrowserHostTabRequest {
                    url: " ".into(),
                    ..tab_request()
                },
                "needs a url",
            ),
            (
                BrowserHostTabRequest {
                    tenant_stream_id: String::new(),
                    ..tab_request()
                },
                "names the stream",
            ),
            (
                BrowserHostTabRequest {
                    width: 0,
                    ..tab_request()
                },
                "0x1080",
            ),
            (
                BrowserHostTabRequest {
                    frame_rate: 120,
                    ..tab_request()
                },
                "not 120",
            ),
            (
                BrowserHostTabRequest {
                    audio_sample_rate: 4_000,
                    ..tab_request()
                },
                "not 4000",
            ),
            (
                BrowserHostTabRequest {
                    frame_slot_count: 0,
                    ..tab_request()
                },
                "not 0",
            ),
            (
                BrowserHostTabRequest {
                    frame_slot_count: MOST_FRAME_SLOTS_PER_TAB + 1,
                    ..tab_request()
                },
                "1 to 8",
            ),
        ] {
            let refusal = broken.refuse_what_cef_cannot_render().expect_err(named);
            assert!(refusal.contains(named), "{refusal}");
        }
    }

    #[test]
    fn a_version_mismatch_names_both_versions() {
        let refusal = refusal_of_another_tab_protocol_version(7);
        assert!(refusal.contains(&format!("protocol {TAB_PROTOCOL_VERSION}")));
        assert!(refusal.contains("speaks 7"));
    }
}

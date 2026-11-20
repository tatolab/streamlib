// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A decoder node's decoder, opened as the node starts and, while the GPU
//! refuses it for want of something another client may free, opened
//! again once a retry interval after each refusal.

use std::time::Instant;

use crate::video_codec_retry_gate::{
    VIDEO_CODEC_RETRY_INTERVAL, VideoCodecRefusal, VideoCodecRetryGate,
};

/// A decoder node's decoder, or the state of its opening.
pub(crate) struct VideoDecoderOpening<D> {
    node_type_name: &'static str,
    decoder: Option<D>,
    retry_gate: VideoCodecRetryGate,
}

impl<D> VideoDecoderOpening<D> {
    pub(crate) fn not_yet_opened(node_type_name: &'static str) -> Self {
        Self {
            node_type_name,
            decoder: None,
            retry_gate: VideoCodecRetryGate::default(),
        }
    }

    /// The decoder, opened with `open` at `now` when none is open and the
    /// last refusal allows a try; `None` while it is not open, and the media
    /// frame at hand is discarded.
    pub(crate) fn the_decoder_at(
        &mut self,
        now: Instant,
        open: impl FnOnce() -> Result<D, VideoCodecRefusal>,
    ) -> Option<&mut D> {
        if self.decoder.is_none() && self.retry_gate.a_try_may_be_made_at(now) {
            match open() {
                Ok(decoder) => {
                    let tries_refused = self.retry_gate.succeeded();
                    if tries_refused > 0 {
                        tracing::info!(
                            tries_refused,
                            "{}: the GPU freed what it refused, so the decoder opened",
                            self.node_type_name
                        );
                    }
                    self.decoder = Some(decoder);
                }
                Err(VideoCodecRefusal::ForNow(refusal)) => {
                    let tries_refused = self.retry_gate.refused_for_now_at(now);
                    if tries_refused.is_power_of_two() {
                        tracing::error!(
                            tries_refused,
                            "{refusal}; media frames are discarded and the decoder is opened \
                             again every {VIDEO_CODEC_RETRY_INTERVAL:?} until the GPU frees what \
                             it refused"
                        );
                    }
                }
                Err(VideoCodecRefusal::ForGood(refusal)) => {
                    self.retry_gate.failed_for_good();
                    tracing::error!("{refusal}; every media frame is discarded");
                }
            }
        }
        self.decoder.as_mut()
    }

    /// The decoder, when one opened.
    pub(crate) fn opened_decoder(&self) -> Option<&D> {
        self.decoder.as_ref()
    }
}

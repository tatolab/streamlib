// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::cell::Cell;
use std::time::{Duration, Instant};

use crate::video_codec_retry_gate::VIDEO_CODEC_RETRY_INTERVAL;
use crate::video_decoder_opening::{VideoDecoderNotOpened, VideoDecoderOpening};

#[derive(Debug, PartialEq)]
struct DecoderStandIn(u32);

#[test]
fn a_decoder_opens_at_the_first_media_frame_and_is_not_opened_again() {
    let mut opening = VideoDecoderOpening::not_yet_opened("H264Decoder");
    let opens = Cell::new(0);
    let now = Instant::now();
    for _ in 0..3 {
        let decoder = opening.the_decoder_at(now, || {
            opens.set(opens.get() + 1);
            Ok(DecoderStandIn(7))
        });
        assert_eq!(decoder, Some(&mut DecoderStandIn(7)));
    }
    assert_eq!(opens.get(), 1);
    assert_eq!(opening.opened_decoder(), Some(&DecoderStandIn(7)));
}

#[test]
fn a_decoder_refused_for_now_opens_once_the_retry_interval_has_passed_and_the_gpu_frees() {
    let mut opening = VideoDecoderOpening::not_yet_opened("H264Decoder");
    let opens = Cell::new(0);
    let refused_at = Instant::now();
    let refuse_for_now = || {
        opens.set(opens.get() + 1);
        Err(VideoDecoderNotOpened::ForNow("BAR1 is full".to_owned()))
    };
    assert!(opening.the_decoder_at(refused_at, refuse_for_now).is_none());
    let within_the_interval = refused_at + VIDEO_CODEC_RETRY_INTERVAL - Duration::from_millis(1);
    assert!(
        opening
            .the_decoder_at(within_the_interval, refuse_for_now)
            .is_none()
    );
    assert_eq!(opens.get(), 1, "no open is tried within the retry interval");

    let after_the_interval = refused_at + VIDEO_CODEC_RETRY_INTERVAL;
    let decoder = opening.the_decoder_at(after_the_interval, || Ok(DecoderStandIn(3)));
    assert_eq!(decoder, Some(&mut DecoderStandIn(3)));
}

#[test]
fn a_decoder_refused_for_good_is_never_opened_again() {
    let mut opening = VideoDecoderOpening::<DecoderStandIn>::not_yet_opened("H264Decoder");
    let refused_at = Instant::now();
    let refused = opening.the_decoder_at(refused_at, || {
        Err(VideoDecoderNotOpened::ForGood(
            "no Vulkan Video decode".to_owned(),
        ))
    });
    assert!(refused.is_none());
    let opens = Cell::new(0);
    let an_hour_later = refused_at + Duration::from_secs(3600);
    assert!(
        opening
            .the_decoder_at(an_hour_later, || {
                opens.set(opens.get() + 1);
                Ok(DecoderStandIn(1))
            })
            .is_none()
    );
    assert_eq!(opens.get(), 0);
    assert!(opening.opened_decoder().is_none());
}

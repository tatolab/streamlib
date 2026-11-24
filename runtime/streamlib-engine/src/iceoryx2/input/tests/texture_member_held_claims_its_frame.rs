// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A Texture member a bundle input holds keeps a claim on its frame, through the claim the
//! process reading the mailboxes reached, until a newer picture replaces it or its link is
//! removed.

use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::json;

use super::member_of_a_bundle_put_at_the_read::{
    a_link_into_a_member_of, a_metered_picture, publish_a_level, the_bundle_frames_read,
    the_picture,
};
use super::*;

/// The frames claimed and not yet let go, by slot and generation.
type FramesClaimed = Arc<Mutex<Vec<(u32, u64)>>>;

/// A claim on one frame, let go of as it drops.
struct ClaimOnOneFrame {
    frame: (u32, u64),
    frames_claimed: FramesClaimed,
}

impl Drop for ClaimOnOneFrame {
    fn drop(&mut self) {
        self.frames_claimed
            .lock()
            .retain(|claimed| *claimed != self.frame);
    }
}

/// Mailboxes claiming each frame they hold through a claim recording it in the list it answers.
fn mailboxes_recording_each_frame_they_claim() -> (InputMailboxesInner, FramesClaimed) {
    let mailboxes = InputMailboxesInner::new();
    let frames_claimed = FramesClaimed::default();
    let recorded_in = Arc::clone(&frames_claimed);
    mailboxes.reach_the_claim_of_the_pooled_gpu_frames_its_bundles_hold(Box::new(move |frame| {
        let claimed = (frame.slot, frame.generation);
        recorded_in.lock().push(claimed);
        Ok(Some(Box::new(ClaimOnOneFrame {
            frame: claimed,
            frames_claimed: Arc::clone(&recorded_in),
        }) as PooledGpuFrameClaimAMessageHolds))
    }));
    (mailboxes, frames_claimed)
}

/// A held picture keeps one claim on its frame however many bundles the level makes with it,
/// a newer picture's claim replaces it, and the claim is let go of when its link is removed.
///
/// Mental-revert: holding the picture's descriptor alone claims nothing, so a quiet picture link
/// leaves every re-sent bundle naming a slot its producer may hand out again.
#[test]
fn a_held_picture_keeps_one_claim_on_its_frame_until_it_is_replaced_or_its_link_removed() {
    let (mailboxes, frames_claimed) = mailboxes_recording_each_frame_they_claim();
    let into_picture = a_link_into_a_member_of(
        &mailboxes,
        &a_metered_picture(),
        "picture",
        WhatTheMembersArePutInto::TheWholeBundle,
    );
    let into_level = a_link_into_a_member_of(
        &mailboxes,
        &a_metered_picture(),
        "level",
        WhatTheMembersArePutInto::TheWholeBundle,
    );

    publish_a_message(&into_picture, an_image_frame(4, 19), &[], 3);
    for (level, stamp) in [(0.25, 5), (0.5, 7), (0.75, 9)] {
        publish_a_level(&into_level, level, stamp);
    }
    the_bundle_frames_read(&mailboxes, &a_metered_picture());
    assert_eq!(*frames_claimed.lock(), vec![(4, 19)]);

    publish_a_message(&into_picture, an_image_frame(5, 20), &[], 11);
    the_bundle_frames_read(&mailboxes, &a_metered_picture());
    assert_eq!(*frames_claimed.lock(), vec![(5, 20)]);

    mailboxes.remove_channel_link("L-picture");
    assert_eq!(*frames_claimed.lock(), Vec::new());
}

/// A picture whose frame the reading process cannot claim is still held and handed on in the
/// bundle, as a picture read off a wire of its own is.
#[test]
fn a_picture_whose_frame_cannot_be_claimed_is_still_handed_on_in_the_bundle() {
    let mailboxes = InputMailboxesInner::new();
    mailboxes.reach_the_claim_of_the_pooled_gpu_frames_its_bundles_hold(Box::new(|_| {
        Err("its slot moved on".to_owned())
    }));
    let into_picture = a_link_into_a_member_of(
        &mailboxes,
        &a_metered_picture(),
        "picture",
        WhatTheMembersArePutInto::TheWholeBundle,
    );
    let into_level = a_link_into_a_member_of(
        &mailboxes,
        &a_metered_picture(),
        "level",
        WhatTheMembersArePutInto::TheWholeBundle,
    );

    publish_a_message(&into_picture, an_image_frame(4, 19), &[], 3);
    publish_a_level(&into_level, 0.5, 5);

    assert_eq!(
        the_bundle_frames_read(&mailboxes, &a_metered_picture()),
        vec![(
            json!({ "level": 0.5 }),
            vec![the_picture(an_image_frame(4, 19))],
            5
        )]
    );
}

/// A picture whose frame is refused a claim lets go of the claim on the frame the picture before
/// it named, which no bundle names any more.
///
/// Mental-revert: keeping the claim held before over a refused one pins a slot no bundle names.
#[test]
fn a_picture_refused_a_claim_lets_go_of_the_claim_on_the_one_before() {
    let mailboxes = InputMailboxesInner::new();
    let frames_claimed = FramesClaimed::default();
    let recorded_in = Arc::clone(&frames_claimed);
    mailboxes.reach_the_claim_of_the_pooled_gpu_frames_its_bundles_hold(Box::new(move |frame| {
        if frame.generation > 19 {
            return Err("its slot moved on".to_owned());
        }
        let claimed = (frame.slot, frame.generation);
        recorded_in.lock().push(claimed);
        Ok(Some(Box::new(ClaimOnOneFrame {
            frame: claimed,
            frames_claimed: Arc::clone(&recorded_in),
        }) as PooledGpuFrameClaimAMessageHolds))
    }));
    let into_picture = a_link_into_a_member_of(
        &mailboxes,
        &a_metered_picture(),
        "picture",
        WhatTheMembersArePutInto::TheWholeBundle,
    );

    publish_a_message(&into_picture, an_image_frame(4, 19), &[], 3);
    the_bundle_frames_read(&mailboxes, &a_metered_picture());
    assert_eq!(*frames_claimed.lock(), vec![(4, 19)]);
    publish_a_message(&into_picture, an_image_frame(4, 20), &[], 5);
    the_bundle_frames_read(&mailboxes, &a_metered_picture());

    assert_eq!(*frames_claimed.lock(), Vec::new());
}

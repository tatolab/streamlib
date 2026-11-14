// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! A decoder's entry into an encoded stream: at a group start, and back to
//! one after every loss, so no picture is built from references it never saw.

/// What to do with one arriving media frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodedVideoGroupEntryDisposition {
    /// Inside an entered group: decode it.
    Decode,
    /// A group start the decoder enters the stream at, the first one or
    /// after a loss.
    EnterAtThisGroupStart,
    /// Outside any entered group: discard it.
    DiscardUntilTheNextGroupStart,
}

/// Admits media frames from a group start, and back to one after every loss.
#[derive(Debug, Default)]
pub struct EncodedVideoGroupEntryGate {
    inside_an_entered_group: bool,
    media_frames_lost_on_the_link: u64,
    media_frames_discarded_awaiting_a_group_start: u64,
    group_starts_entered_at: u64,
}

impl EncodedVideoGroupEntryGate {
    /// Decide one media frame from its group flag and the loss its read
    /// reported.
    pub fn admit_media_frame(
        &mut self,
        starts_group: bool,
        lost_since_last_read: u64,
    ) -> EncodedVideoGroupEntryDisposition {
        if lost_since_last_read > 0 {
            self.media_frames_lost_on_the_link += lost_since_last_read;
            self.inside_an_entered_group = false;
        }
        match (self.inside_an_entered_group, starts_group) {
            (true, _) => EncodedVideoGroupEntryDisposition::Decode,
            (false, true) => {
                self.inside_an_entered_group = true;
                self.group_starts_entered_at += 1;
                EncodedVideoGroupEntryDisposition::EnterAtThisGroupStart
            }
            (false, false) => {
                self.media_frames_discarded_awaiting_a_group_start += 1;
                EncodedVideoGroupEntryDisposition::DiscardUntilTheNextGroupStart
            }
        }
    }

    /// Give up the entered group: a media frame in it failed to decode, so
    /// what follows references a picture the decoder does not have.
    pub fn discard_the_rest_of_the_current_group(&mut self) {
        self.inside_an_entered_group = false;
    }

    pub fn media_frames_lost_on_the_link(&self) -> u64 {
        self.media_frames_lost_on_the_link
    }

    pub fn media_frames_discarded_awaiting_a_group_start(&self) -> u64 {
        self.media_frames_discarded_awaiting_a_group_start
    }

    /// Times the decoder entered the stream, the first entry included.
    pub fn group_starts_entered_at(&self) -> u64 {
        self.group_starts_entered_at
    }
}

#[cfg(test)]
mod tests {
    use super::EncodedVideoGroupEntryDisposition::{
        Decode, DiscardUntilTheNextGroupStart, EnterAtThisGroupStart,
    };
    use super::*;

    #[test]
    fn a_decoder_attaching_mid_group_discards_until_the_first_group_start() {
        let mut gate = EncodedVideoGroupEntryGate::default();
        assert_eq!(
            gate.admit_media_frame(false, 0),
            DiscardUntilTheNextGroupStart
        );
        assert_eq!(gate.admit_media_frame(true, 0), EnterAtThisGroupStart);
        assert_eq!(gate.admit_media_frame(false, 0), Decode);
        assert_eq!(gate.media_frames_discarded_awaiting_a_group_start(), 1);
    }

    #[test]
    fn a_loss_discards_to_the_next_group_start() {
        let mut gate = EncodedVideoGroupEntryGate::default();
        gate.admit_media_frame(true, 0);
        assert_eq!(
            gate.admit_media_frame(false, 2),
            DiscardUntilTheNextGroupStart
        );
        assert_eq!(
            gate.admit_media_frame(false, 0),
            DiscardUntilTheNextGroupStart
        );
        assert_eq!(gate.admit_media_frame(true, 0), EnterAtThisGroupStart);
        assert_eq!(gate.media_frames_lost_on_the_link(), 2);
        assert_eq!(gate.group_starts_entered_at(), 2);
    }

    #[test]
    fn a_failed_decode_gives_up_the_group() {
        let mut gate = EncodedVideoGroupEntryGate::default();
        gate.admit_media_frame(true, 0);
        gate.discard_the_rest_of_the_current_group();
        assert_eq!(
            gate.admit_media_frame(false, 0),
            DiscardUntilTheNextGroupStart
        );
    }
}

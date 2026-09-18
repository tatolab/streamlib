// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! How long a subscriber with no listener slot sleeps after an empty poll.
//!
//! A channel's notify service is destination-keyed and sized to its fan-in, so
//! a reader that is not one of the channel's destinations — the tap, and the
//! mesh egress — has no fd to wait on and polls the ring instead. The two share
//! this one backoff rather than each choosing its own cadence.

use std::time::{Duration, Instant};

/// The first sleep after a poll finds the channel ring empty. Short enough that
/// a live channel's next bag is picked up immediately.
pub const CHANNEL_SHORTEST_IDLE_POLL_BACKOFF: Duration = Duration::from_micros(500);

/// The longest sleep the backoff climbs to once a channel has gone quiet.
///
/// The floor alone costs about 2,000 wake-ups a second for as long as a reader
/// is attached to a channel carrying nothing. Climbing to here bounds that at
/// about fifty, and bounds what it costs: a channel that starts carrying again
/// is drained within this, and a detach arriving mid-sleep is noticed within it
/// — the reader reads its stop flag once per loop.
pub const CHANNEL_LONGEST_IDLE_POLL_BACKOFF: Duration = Duration::from_millis(20);

/// How long a channel must carry nothing before the backoff starts climbing.
///
/// Held well above any ordinary inter-bag gap on purpose. Climbing from the
/// first empty poll would reach the ceiling inside a single 30 fps frame
/// interval, so every frame on a live channel would be observed up to a ceiling
/// late — a latency cost paid by exactly the readers that are not idle. Past
/// this, a channel is quiet rather than merely between bags, and nobody is
/// waiting on a cadence.
pub const CHANNEL_QUIET_BEFORE_THE_BACKOFF_CLIMBS: Duration = Duration::from_millis(250);

/// How long the reading thread sleeps after a poll that found nothing: the
/// floor until the channel has been quiet a while, then climbing to the ceiling,
/// and back to the floor the moment a bag arrives.
///
/// A reader's wake-up rate follows the channel it reads rather than the clock. A
/// channel carrying at any ordinary cadence never leaves the floor, because the
/// gap between its bags never reaches [`QUIET_BEFORE_THE_BACKOFF_CLIMBS`] — so
/// the saving is taken from idle readers only, and a live one is read exactly as
/// promptly as before.
#[derive(Debug)]
pub struct ChannelIdlePollBackoff {
    /// When this quiet stretch began, on the machine's monotonic clock. `None`
    /// until the first empty poll of the stretch marks it.
    quiet_since: Option<Instant>,
    /// The sleep the next empty poll takes.
    next_sleep: Duration,
}

impl ChannelIdlePollBackoff {
    pub fn starting_at_the_shortest_sleep() -> Self {
        Self {
            quiet_since: None,
            next_sleep: CHANNEL_SHORTEST_IDLE_POLL_BACKOFF,
        }
    }

    /// The sleep this empty poll earns, climbing for the next one once the
    /// channel has been quiet past [`QUIET_BEFORE_THE_BACKOFF_CLIMBS`].
    ///
    /// `polled_at` is read from the monotonic clock by the caller rather than
    /// summed from the sleeps taken, which would undercount by every scheduler
    /// delay and let a channel stay at the floor well past the threshold.
    pub fn sleep_this_empty_poll_earns(&mut self, polled_at: Instant) -> Duration {
        let quiet_since = *self.quiet_since.get_or_insert(polled_at);
        let sleeping_for = self.next_sleep;
        if polled_at.saturating_duration_since(quiet_since)
            >= CHANNEL_QUIET_BEFORE_THE_BACKOFF_CLIMBS
        {
            self.next_sleep = (self.next_sleep * 2).min(CHANNEL_LONGEST_IDLE_POLL_BACKOFF);
        }
        sleeping_for
    }

    /// Back to the floor: the channel is carrying, so neither this quiet stretch
    /// nor the sleep it had grown to outlives the bag that ended it.
    pub fn reset_after_a_bag_arrived(&mut self) {
        *self = Self::starting_at_the_shortest_sleep();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The floor holds through an ordinary inter-bag gap and the sleep climbs
    /// only once a channel is genuinely quiet.
    ///
    /// What it catches, and the reason the threshold exists at all: a backoff
    /// that climbs from the first empty poll sends a 30 fps channel to the
    /// ceiling inside every frame interval — so a LIVE reader pays the latency
    /// the backoff was supposed to charge idle ones.
    #[test]
    fn the_idle_backoff_holds_the_floor_through_an_ordinary_gap_and_climbs_only_once_quiet() {
        let mut backoff = ChannelIdlePollBackoff::starting_at_the_shortest_sleep();
        // The clock the reader reads, advanced here by exactly the sleeps the
        // backoff asks for, so the policy is pinned without the test sleeping.
        let mut polled_at = Instant::now();

        // A 30 fps inter-frame gap: every poll inside it must still be at the
        // floor, so a live channel is read exactly as promptly as before.
        let one_frame_at_thirty_fps = Duration::from_micros(33_333);
        let quiet_began_at = polled_at;
        while polled_at.duration_since(quiet_began_at) < one_frame_at_thirty_fps {
            let sleeping_for = backoff.sleep_this_empty_poll_earns(polled_at);
            assert_eq!(
                sleeping_for,
                CHANNEL_SHORTEST_IDLE_POLL_BACKOFF,
                "a poll {:?} into an ordinary gap must stay at the floor",
                polled_at.duration_since(quiet_began_at)
            );
            polled_at += sleeping_for;
        }

        // Past the quiet threshold it climbs, and never past the ceiling.
        while polled_at.duration_since(quiet_began_at) < CHANNEL_QUIET_BEFORE_THE_BACKOFF_CLIMBS * 4
        {
            let sleeping_for = backoff.sleep_this_empty_poll_earns(polled_at);
            assert!(
                sleeping_for <= CHANNEL_LONGEST_IDLE_POLL_BACKOFF,
                "an idle sleep of {sleeping_for:?} is past the ceiling"
            );
            polled_at += sleeping_for;
        }
        assert_eq!(
            backoff.sleep_this_empty_poll_earns(polled_at),
            CHANNEL_LONGEST_IDLE_POLL_BACKOFF,
            "a long-quiet channel settles at the ceiling"
        );

        backoff.reset_after_a_bag_arrived();
        assert_eq!(
            backoff.sleep_this_empty_poll_earns(polled_at),
            CHANNEL_SHORTEST_IDLE_POLL_BACKOFF,
            "the bag that ends a quiet stretch must not leave its sleep behind"
        );
        assert_eq!(
            backoff.sleep_this_empty_poll_earns(polled_at + one_frame_at_thirty_fps),
            CHANNEL_SHORTEST_IDLE_POLL_BACKOFF,
            "and the stretch it timed must not outlive it either"
        );
    }
}

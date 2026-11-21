// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Pool slots whose producer is gone while a reader still holds one of their frames. A node
//! rebuilt while its stream plays tears its producer down, and a node downstream may keep the
//! last frame it read across ticks and bind it into each draw. Each such slot stays registered,
//! so that frame still resolves, until nothing holds it; every slot nothing holds is released
//! at once.

use parking_lot::Mutex;

use crate::core::context::GpuContextLimitedAccess;

/// The slots one producer left behind that a reader still held when it went.
pub(crate) trait SlotsAwaitingTheirReaders: Send {
    /// Release, through `sandbox`, each of the slots nothing holds any longer, answering
    /// whether any is still held.
    fn release_each_one_nobody_holds(&mut self, sandbox: &GpuContextLimitedAccess) -> bool;

    /// Release every one of the slots, held or not, as the stream they served stops and its
    /// readers with it.
    fn release_every_one(self: Box<Self>, sandbox: &GpuContextLimitedAccess);
}

/// Every producer's slots awaiting their readers, across the nodes of one loaded stream.
#[derive(Default)]
pub(crate) struct SlotsOutlivingTheirProducers {
    awaiting_their_readers: Mutex<Vec<Box<dyn SlotsAwaitingTheirReaders>>>,
}

impl SlotsOutlivingTheirProducers {
    /// Keep `slots` registered until their readers let go of them.
    pub(crate) fn keep_until_their_readers_let_go(
        &self,
        slots: Box<dyn SlotsAwaitingTheirReaders>,
    ) {
        self.awaiting_their_readers.lock().push(slots);
    }

    /// Release, through `sandbox`, each slot kept here that nothing holds any longer. A sweep
    /// another caller is already running answers for this one, so a producer's hand-off never
    /// waits behind it.
    pub(crate) fn release_each_slot_nobody_holds(&self, sandbox: &GpuContextLimitedAccess) {
        let Some(mut awaiting_their_readers) = self.awaiting_their_readers.try_lock() else {
            return;
        };
        awaiting_their_readers.retain_mut(|slots| slots.release_each_one_nobody_holds(sandbox));
    }

    /// Release, through `sandbox`, every slot kept here, held or not.
    pub(crate) fn release_every_slot(&self, sandbox: &GpuContextLimitedAccess) {
        let awaiting_their_readers = std::mem::take(&mut *self.awaiting_their_readers.lock());
        for slots in awaiting_their_readers {
            slots.release_every_one(sandbox);
        }
    }

    /// How many producers' slots are kept here, awaiting their readers.
    #[cfg(test)]
    pub(crate) fn producers_awaiting_their_readers(&self) -> usize {
        self.awaiting_their_readers.lock().len()
    }
}

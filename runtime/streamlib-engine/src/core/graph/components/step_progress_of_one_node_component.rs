// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use streamlib_ipc_types::HelperProcessStepProgressBoardEntry;

/// What one node has done in its stream's steps: its entry on the step ledger
/// the step coordinator reads before it closes a step.
#[derive(Debug, Default)]
pub struct StepProgressOfOneNode {
    finishes_every_step: AtomicBool,
    panicked_mid_callback: AtomicBool,
    steps_finished: AtomicU64,
    callbacks_in_flight: AtomicU32,
}

/// One node's step-ledger entry as it stood when read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StepProgressReadingOfOneNode {
    /// Whether the node paces itself on stream time, and so must finish every
    /// step before it closes.
    pub finishes_every_step: bool,
    /// How many steps the node finished; step `k` is finished once this
    /// exceeds `k`.
    pub steps_finished: u64,
    /// Callbacks of the node's started and not yet returned.
    pub callbacks_in_flight: u32,
    /// Whether a callback of the node's panicked, so it will never finish one
    /// again.
    pub panicked_mid_callback: bool,
}

impl StepProgressReadingOfOneNode {
    /// Whether this node lets step `step_index` close: idle, and finished with
    /// the step when it must finish every one.
    pub fn lets_the_step_close(&self, step_index: u64) -> bool {
        self.callbacks_in_flight == 0
            && (!self.finishes_every_step || self.steps_finished > step_index)
    }

    /// This reading with what the node's helper process wrote of its callbacks
    /// added: the node runs there, and its host here.
    pub fn with_what_its_helper_process_wrote(
        self,
        written_by_its_helper_process: HelperProcessStepProgressBoardEntry,
    ) -> Self {
        Self {
            steps_finished: self
                .steps_finished
                .max(written_by_its_helper_process.steps_finished),
            callbacks_in_flight: self
                .callbacks_in_flight
                .saturating_add(written_by_its_helper_process.callbacks_in_flight),
            ..self
        }
    }
}

impl StepProgressOfOneNode {
    /// Note that this node paces itself on stream time, so a step closes only
    /// once it finished it.
    pub fn note_it_finishes_every_step(&self) {
        self.finishes_every_step.store(true, Ordering::Release);
    }

    /// Note a callback started, until the returned guard drops; a step cannot
    /// close while it runs, and a panic out of it is noted as the node's end.
    pub fn callback_in_flight(&self) -> StepProgressCallbackInFlight<'_> {
        self.note_a_callback_started();
        StepProgressCallbackInFlight {
            step_progress: self,
        }
    }

    fn note_a_callback_started(&self) {
        self.callbacks_in_flight.fetch_add(1, Ordering::AcqRel);
    }

    fn note_a_callback_returned(&self) {
        let _ = self.callbacks_in_flight.try_update(
            Ordering::AcqRel,
            Ordering::Acquire,
            |callbacks_in_flight| Some(callbacks_in_flight.saturating_sub(1)),
        );
    }

    /// Note this node finished step `step_index`.
    pub fn note_step_finished(&self, step_index: u64) {
        self.steps_finished
            .fetch_max(step_index.saturating_add(1), Ordering::AcqRel);
    }

    /// This node's entry as it stands now.
    pub fn reading(&self) -> StepProgressReadingOfOneNode {
        StepProgressReadingOfOneNode {
            finishes_every_step: self.finishes_every_step.load(Ordering::Acquire),
            steps_finished: self.steps_finished.load(Ordering::Acquire),
            callbacks_in_flight: self.callbacks_in_flight.load(Ordering::Acquire),
            panicked_mid_callback: self.panicked_mid_callback.load(Ordering::Acquire),
        }
    }
}

/// One callback of a node's, counted in flight on its step-ledger entry for as
/// long as this lives.
#[must_use = "the callback counts in flight only while this guard lives"]
pub struct StepProgressCallbackInFlight<'a> {
    step_progress: &'a StepProgressOfOneNode,
}

impl Drop for StepProgressCallbackInFlight<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.step_progress
                .panicked_mid_callback
                .store(true, Ordering::Release);
        }
        self.step_progress.note_a_callback_returned();
    }
}

/// A processor node's [`StepProgressOfOneNode`], shared with its runner.
#[derive(Debug, Default, Clone)]
pub struct StepProgressOfOneNodeComponent(pub Arc<StepProgressOfOneNode>);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_node_in_a_callback_holds_the_step_open() {
        let progress = StepProgressOfOneNode::default();
        let callback = progress.callback_in_flight();
        assert!(!progress.reading().lets_the_step_close(0));
        drop(callback);
        assert!(progress.reading().lets_the_step_close(0));
        assert!(!progress.reading().panicked_mid_callback);
    }

    #[test]
    fn a_callback_that_panics_is_noted_as_the_nodes_end() {
        let progress = StepProgressOfOneNode::default();
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _callback = progress.callback_in_flight();
            panic!("a callback panicked");
        }));
        assert!(panicked.is_err());
        assert!(progress.reading().panicked_mid_callback);
        assert_eq!(progress.reading().callbacks_in_flight, 0);
    }

    #[test]
    fn a_helper_placed_node_holds_the_step_open_while_its_helper_is_in_a_callback() {
        let progress = StepProgressOfOneNode::default();
        progress.note_it_finishes_every_step();
        let in_a_callback = progress.reading().with_what_its_helper_process_wrote(
            HelperProcessStepProgressBoardEntry {
                steps_finished: 3,
                callbacks_in_flight: 1,
                reserved: 0,
            },
        );
        assert!(!in_a_callback.lets_the_step_close(2));
        let returned = progress.reading().with_what_its_helper_process_wrote(
            HelperProcessStepProgressBoardEntry {
                steps_finished: 3,
                callbacks_in_flight: 0,
                reserved: 0,
            },
        );
        assert!(returned.lets_the_step_close(2));
        assert!(!returned.lets_the_step_close(3));
    }

    #[test]
    fn a_node_that_finishes_every_step_holds_each_open_until_it_finished_it() {
        let progress = StepProgressOfOneNode::default();
        progress.note_it_finishes_every_step();
        assert!(!progress.reading().lets_the_step_close(0));
        progress.note_step_finished(0);
        assert!(progress.reading().lets_the_step_close(0));
        assert!(!progress.reading().lets_the_step_close(1));
        progress.note_step_finished(1);
        progress.note_step_finished(0);
        assert_eq!(progress.reading().steps_finished, 2);
    }
}

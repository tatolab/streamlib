// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Lifecycle replies matched to their commands by id, over the queue the
//! bridge reader fills, with no helper process and no GPU.

use std::sync::mpsc;
use std::time::Duration;

use super::*;

const PROMPT: Duration = Duration::from_millis(200);

struct LifecycleRepliesWithTheReaderEndInHand {
    replies: LifecycleRepliesMatchedToTheirCommands,
    reader_end: mpsc::Sender<serde_json::Value>,
    next_command_id: u64,
}

impl LifecycleRepliesWithTheReaderEndInHand {
    fn new() -> Self {
        let (reader_end, replies) = mpsc::channel();
        Self {
            replies: LifecycleRepliesMatchedToTheirCommands::arriving_on(replies),
            reader_end,
            next_command_id: 1,
        }
    }

    fn send(&mut self, command: LifecycleCommandAwaitingItsReply) -> LifecycleCommandSent {
        let sent = LifecycleCommandSent {
            command,
            command_id: OutOfProcessCommandId(self.next_command_id),
        };
        self.next_command_id += 1;
        self.replies.owe_a_reply_to(sent);
        sent
    }

    fn helper_answers(&self, sent: LifecycleCommandSent, rpc: &str) {
        self.reader_end
            .send(serde_json::json!({"rpc": rpc, "command_id": sent.command_id.0}))
            .expect("the queue is open");
    }

    fn await_the_reply_to(
        &self,
        awaited: LifecycleCommandSent,
    ) -> std::result::Result<serde_json::Value, LifecycleReplyNotReceived> {
        self.replies.await_the_reply_to(awaited, PROMPT, "p-test")
    }
}

#[test]
fn a_reply_carrying_the_awaited_commands_id_is_its_answer() {
    let mut bridge = LifecycleRepliesWithTheReaderEndInHand::new();
    let setup = bridge.send(LifecycleCommandAwaitingItsReply::Setup);
    bridge.helper_answers(setup, "ready");

    let reply = bridge
        .await_the_reply_to(setup)
        .expect("the reply to setup");
    assert_eq!(reply["rpc"], "ready");
}

/// `stop` and `teardown` go out together, and the ladder waits on `teardown`
/// whether or not `stop` was answered. A `stopped` arriving while `teardown`
/// is awaited answers a command its waiter abandoned, so it is passed over by
/// its id rather than read as the teardown.
///
/// Fail-without-fix: take whatever arrives first, as arrival order did, and
/// the late `stopped` is read as the answer to `teardown`.
#[test]
fn a_late_reply_to_an_abandoned_command_is_passed_over_for_the_awaited_one() {
    let mut bridge = LifecycleRepliesWithTheReaderEndInHand::new();
    let stop = bridge.send(LifecycleCommandAwaitingItsReply::Stop);
    let teardown = bridge.send(LifecycleCommandAwaitingItsReply::Teardown);
    assert!(matches!(
        bridge.await_the_reply_to(stop),
        Err(LifecycleReplyNotReceived::DeadlinePassed { .. })
    ));
    bridge.replies.abandon_the_reply_to(stop);
    bridge.helper_answers(stop, "stopped");
    bridge.helper_answers(teardown, "done");

    let reply = bridge
        .await_the_reply_to(teardown)
        .expect("the reply to teardown");
    assert_eq!(reply["rpc"], "done");
}

/// A reply to another command still awaited means the two sides no longer
/// agree on what was asked; it is an error naming both commands, never the
/// awaited command's answer.
#[test]
fn a_reply_to_another_command_still_owed_one_is_an_error_naming_both() {
    let mut bridge = LifecycleRepliesWithTheReaderEndInHand::new();
    let on_pause = bridge.send(LifecycleCommandAwaitingItsReply::OnPause);
    let carry_links = bridge.send(LifecycleCommandAwaitingItsReply::CarryLinks);
    bridge.helper_answers(on_pause, "ok");

    let out_of_step = bridge
        .await_the_reply_to(carry_links)
        .expect_err("the reply to on_pause is not the answer to carry_links");
    assert_eq!(
        out_of_step,
        LifecycleReplyNotReceived::AnsweredAnotherCommand {
            awaited: carry_links,
            answered: on_pause,
        }
    );
    let message = out_of_step.to_string();
    assert!(
        message.contains("`on_pause` (command 1)") && message.contains("`carry_links` (command 2)"),
        "the error names both commands: {message}"
    );
}

#[test]
fn a_reply_to_a_command_never_sent_is_an_error() {
    let mut bridge = LifecycleRepliesWithTheReaderEndInHand::new();
    let setup = bridge.send(LifecycleCommandAwaitingItsReply::Setup);
    bridge
        .reader_end
        .send(serde_json::json!({"rpc": "ready", "command_id": 99}))
        .expect("the queue is open");

    assert_eq!(
        bridge.await_the_reply_to(setup),
        Err(LifecycleReplyNotReceived::AnsweredACommandNotAwaited {
            awaited: setup,
            answered_command_id: OutOfProcessCommandId(99),
        })
    );
}

/// A reply that carries no id cannot be matched to any command, so it is
/// never taken as the answer to whichever happens to be awaited.
#[test]
fn a_reply_naming_no_command_is_an_error() {
    let mut bridge = LifecycleRepliesWithTheReaderEndInHand::new();
    let teardown = bridge.send(LifecycleCommandAwaitingItsReply::Teardown);
    bridge
        .reader_end
        .send(serde_json::json!({"rpc": "done"}))
        .expect("the queue is open");

    assert_eq!(
        bridge.await_the_reply_to(teardown),
        Err(LifecycleReplyNotReceived::ReplyNamedNoCommand { awaited: teardown })
    );
}

#[test]
fn a_closed_queue_says_the_helper_can_no_longer_answer() {
    let mut bridge = LifecycleRepliesWithTheReaderEndInHand::new();
    let setup = bridge.send(LifecycleCommandAwaitingItsReply::Setup);
    let LifecycleRepliesWithTheReaderEndInHand {
        replies,
        reader_end,
        ..
    } = bridge;
    drop(reader_end);

    assert_eq!(
        replies.await_the_reply_to(setup, PROMPT, "p-test"),
        Err(LifecycleReplyNotReceived::HelperProcessCanNoLongerAnswer { awaited: setup })
    );
}

/// Shutdown abandons whatever was still awaited, so a `setup` cut short by it
/// answers after `stop` is on the wire without being read as the stop.
#[test]
fn abandoning_every_command_owed_a_reply_passes_over_a_setup_cut_short() {
    let mut bridge = LifecycleRepliesWithTheReaderEndInHand::new();
    let setup = bridge.send(LifecycleCommandAwaitingItsReply::Setup);
    bridge
        .replies
        .abandon_the_reply_to_every_command_still_owed_one();
    let stop = bridge.send(LifecycleCommandAwaitingItsReply::Stop);
    bridge.helper_answers(setup, "error");
    bridge.helper_answers(stop, "stopped");

    let reply = bridge.await_the_reply_to(stop).expect("the reply to stop");
    assert_eq!(reply["rpc"], "stopped");
}

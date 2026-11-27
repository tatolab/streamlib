// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Lifecycle commands written over a real socket pair and their replies matched
//! by id, with the queue the bridge reader fills in hand and no GPU.

use std::io::BufReader;
use std::sync::mpsc;
use std::time::Duration;

use super::*;

const PROMPT: Duration = Duration::from_millis(200);

/// The parent's lifecycle commands to one helper, with the helper's end of the
/// socket and the reader's end of the reply queue in hand.
struct LifecycleCommandsWithTheHelperEndInHand {
    commands: LifecycleCommandsToOneSubprocess,
    helper_end: BufReader<UnixStream>,
    reader_end: mpsc::Sender<serde_json::Value>,
}

impl LifecycleCommandsWithTheHelperEndInHand {
    fn new() -> Self {
        let (parent_end, helper_end) = UnixStream::pair().expect("socketpair");
        let parent_side = Arc::new(
            ParentSideOfOneSubprocessBridgeSharedByItsThreads::over(
                parent_end,
                "p-test".to_owned(),
            )
            .expect("the parent side"),
        );
        let (reader_end, replies) = mpsc::channel();
        Self {
            commands: LifecycleCommandsToOneSubprocess::over(parent_side, replies),
            helper_end: BufReader::new(helper_end),
            reader_end,
        }
    }

    fn send(&self, command: LifecycleCommandAnsweredByAReply) -> LifecycleCommandSent {
        self.commands
            .send_a_lifecycle_command_awaiting_its_reply(command, LifecycleCommandFields::new())
            .expect("the command is written")
    }

    /// Read the next frame the helper was sent, as the helper would.
    fn helper_reads(&mut self, cmd: &str) -> serde_json::Value {
        let frame = read_frame(&mut self.helper_end).expect("a frame reached the helper");
        assert_eq!(frame["cmd"], cmd);
        frame
    }

    /// Answer `frame` with `rpc`, echoing its id as the helper does.
    fn helper_answers(&self, frame: &serde_json::Value, rpc: &str) {
        self.reader_end
            .send(serde_json::json!({"rpc": rpc, "command_id": frame["command_id"]}))
            .expect("the queue is open");
    }

    fn await_the_reply_to(
        &self,
        awaited: LifecycleCommandSent,
    ) -> std::result::Result<serde_json::Value, LifecycleReplyNotReceived> {
        self.commands.await_the_reply_to(awaited, PROMPT)
    }
}

#[test]
fn a_command_is_written_carrying_the_id_its_reply_is_matched_to() {
    let mut bridge = LifecycleCommandsWithTheHelperEndInHand::new();
    let setup = bridge.send(LifecycleCommandAnsweredByAReply::Setup);
    let frame = bridge.helper_reads("setup");
    assert_eq!(frame["command_id"], serde_json::json!(setup.command_id));
    bridge.helper_answers(&frame, "ready");

    let reply = bridge
        .await_the_reply_to(setup)
        .expect("the reply to setup");
    assert_eq!(reply["rpc"], "ready");
}

/// A helper slow to answer `setup` is shut down while it is still setting up:
/// it answers `setup`, then `stop`, both after their waits ended, then
/// `teardown`. Each rung reads only its own command's reply.
///
/// Fail-without-fix: drop either abandon in the shutdown path, and the late
/// `setup` or `stopped` reply is an error naming the wrong command, so the
/// ladder reads the helper as out of step instead of torn down.
#[test]
fn a_shutdown_cut_into_a_slow_setup_reads_each_rung_answered_by_its_own_reply() {
    let mut bridge = LifecycleCommandsWithTheHelperEndInHand::new();
    let setup = bridge.send(LifecycleCommandAnsweredByAReply::Setup);
    let setup_frame = bridge.helper_reads("setup");
    assert!(matches!(
        bridge.await_the_reply_to(setup),
        Err(LifecycleReplyNotReceived::DeadlinePassed { .. })
    ));

    let mut shutdown_commands_sent = ShutdownCommandsSent::default();
    bridge
        .commands
        .send_the_shutdown_commands(&mut shutdown_commands_sent)
        .expect("both shutdown commands are written");
    let stop_frame = bridge.helper_reads("stop");
    let teardown_frame = bridge.helper_reads("teardown");
    assert_eq!(stop_frame["capability"], "full");
    bridge.helper_answers(&setup_frame, "ready");

    assert!(matches!(
        bridge.commands.await_the_reply_to_a_shutdown_command(
            &shutdown_commands_sent,
            HelperProcessShutdownCommand::Stop,
            PROMPT,
        ),
        Err(LifecycleReplyNotReceived::DeadlinePassed { .. })
    ));
    bridge.helper_answers(&stop_frame, "stopped");
    bridge.helper_answers(&teardown_frame, "done");

    let reply = bridge
        .commands
        .await_the_reply_to_a_shutdown_command(
            &shutdown_commands_sent,
            HelperProcessShutdownCommand::Teardown,
            PROMPT,
        )
        .expect("the reply to teardown");
    assert_eq!(reply["rpc"], "done");
}

/// A helper that skipped its `stop` reply answers `teardown` while the stop
/// rung still waits: out of step for `stop`, and still the answer to
/// `teardown` once the ladder waits on it.
///
/// Fail-without-fix: drop the reply once it is read as out of step, and the
/// teardown rung waits out its whole budget for a reply that already came.
#[test]
fn a_reply_read_out_of_step_is_kept_for_the_command_it_answers() {
    let mut bridge = LifecycleCommandsWithTheHelperEndInHand::new();
    let mut shutdown_commands_sent = ShutdownCommandsSent::default();
    bridge
        .commands
        .send_the_shutdown_commands(&mut shutdown_commands_sent)
        .expect("both shutdown commands are written");
    bridge.helper_reads("stop");
    let teardown_frame = bridge.helper_reads("teardown");
    bridge.helper_answers(&teardown_frame, "done");

    assert!(matches!(
        bridge.commands.await_the_reply_to_a_shutdown_command(
            &shutdown_commands_sent,
            HelperProcessShutdownCommand::Stop,
            PROMPT,
        ),
        Err(LifecycleReplyNotReceived::AnsweredAnotherCommand { .. })
    ));
    let reply = bridge
        .commands
        .await_the_reply_to_a_shutdown_command(
            &shutdown_commands_sent,
            HelperProcessShutdownCommand::Teardown,
            PROMPT,
        )
        .expect("the reply to teardown, read while stop was awaited");
    assert_eq!(reply["rpc"], "done");
}

#[test]
fn awaiting_a_shutdown_command_never_sent_names_it() {
    let bridge = LifecycleCommandsWithTheHelperEndInHand::new();
    assert_eq!(
        bridge.commands.await_the_reply_to_a_shutdown_command(
            &ShutdownCommandsSent::default(),
            HelperProcessShutdownCommand::Teardown,
            PROMPT,
        ),
        Err(LifecycleReplyNotReceived::ShutdownCommandNeverSent {
            command: HelperProcessShutdownCommand::Teardown,
        })
    );
}

/// A reply to another command still awaited means the two sides no longer
/// agree on what was asked; it is an error naming both commands, never the
/// awaited command's answer.
#[test]
fn a_reply_to_another_command_still_owed_one_is_an_error_naming_both() {
    let mut bridge = LifecycleCommandsWithTheHelperEndInHand::new();
    let on_pause = bridge.send(LifecycleCommandAnsweredByAReply::OnPause);
    let carry_links = bridge.send(LifecycleCommandAnsweredByAReply::CarryLinks);
    let on_pause_frame = bridge.helper_reads("on_pause");
    bridge.helper_answers(&on_pause_frame, "ok");

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
    let bridge = LifecycleCommandsWithTheHelperEndInHand::new();
    let setup = bridge.send(LifecycleCommandAnsweredByAReply::Setup);
    bridge
        .reader_end
        .send(serde_json::json!({"rpc": "ready", "command_id": 99}))
        .expect("the queue is open");

    assert_eq!(
        bridge.await_the_reply_to(setup),
        Err(LifecycleReplyNotReceived::AnsweredACommandNotAwaited {
            awaited: setup,
            answered_command_id: OutOfProcessCommandId::from_the_wire(99),
        })
    );
}

/// A reply that carries no id cannot be matched to any command, so it is
/// never taken as the answer to whichever happens to be awaited.
#[test]
fn a_reply_naming_no_command_is_an_error() {
    let bridge = LifecycleCommandsWithTheHelperEndInHand::new();
    let teardown = bridge.send(LifecycleCommandAnsweredByAReply::Teardown);
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
    let bridge = LifecycleCommandsWithTheHelperEndInHand::new();
    let setup = bridge.send(LifecycleCommandAnsweredByAReply::Setup);
    let LifecycleCommandsWithTheHelperEndInHand {
        commands,
        reader_end,
        ..
    } = bridge;
    drop(reader_end);

    assert_eq!(
        commands.await_the_reply_to(setup, PROMPT),
        Err(LifecycleReplyNotReceived::HelperProcessCanNoLongerAnswer { awaited: setup })
    );
}

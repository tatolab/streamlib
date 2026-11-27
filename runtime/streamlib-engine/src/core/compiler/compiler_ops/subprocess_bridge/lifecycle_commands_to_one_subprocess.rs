// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The lifecycle commands the parent sends one helper, and their replies, each
//! matched to its command by the [`OutOfProcessCommandId`] it echoes.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::core::error::{Error, Result};
use crate::core::nodes::OutOfProcessCommandId;

use super::{
    HelperProcessShutdownCommand, OUT_OF_PROCESS_COMMAND_ID_FIELD,
    ParentSideOfOneSubprocessBridgeSharedByItsThreads, out_of_process_command_id_of,
};

/// A lifecycle command the parent sends a helper and waits on the reply to.
///
/// `run`, `unwire_link`, `unfeed_island` and `set_input_values` are absent:
/// they are answered by nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LifecycleCommandAnsweredByAReply {
    Setup,
    Stop,
    Teardown,
    OnPause,
    OnResume,
    CarryLinks,
}

impl LifecycleCommandAnsweredByAReply {
    /// What the parent writes as the frame's `cmd`.
    pub(crate) fn command_tag(self) -> &'static str {
        match self {
            Self::Setup => super::SETUP_LIFECYCLE_COMMAND_TO_HELPER_PROCESS,
            Self::Stop => "stop",
            Self::Teardown => "teardown",
            Self::OnPause => "on_pause",
            Self::OnResume => "on_resume",
            Self::CarryLinks => "carry_links",
        }
    }
}

/// The fields of one lifecycle command beside its `cmd` and `command_id`.
pub(crate) type LifecycleCommandFields = serde_json::Map<String, serde_json::Value>;

/// The fields `fields` name, as one lifecycle command carries them.
pub(crate) fn lifecycle_command_fields<const FIELD_COUNT: usize>(
    fields: [(&str, serde_json::Value); FIELD_COUNT],
) -> LifecycleCommandFields {
    fields
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value))
        .collect()
}

/// One lifecycle command sent, by the id its reply is matched to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LifecycleCommandSent {
    pub(crate) command: LifecycleCommandAnsweredByAReply,
    pub(crate) command_id: OutOfProcessCommandId,
}

impl std::fmt::Display for LifecycleCommandSent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "`{}` (command {})",
            self.command.command_tag(),
            self.command_id
        )
    }
}

/// Why the reply to one lifecycle command did not arrive.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum LifecycleReplyNotReceived {
    #[error("the helper process did not answer {awaited} in time")]
    DeadlinePassed { awaited: LifecycleCommandSent },
    #[error("the helper process can no longer answer {awaited}: its bridge has closed")]
    HelperProcessCanNoLongerAnswer { awaited: LifecycleCommandSent },
    #[error(
        "the helper process answered {answered} while the parent waited on its answer to \
         {awaited}"
    )]
    AnsweredAnotherCommand {
        awaited: LifecycleCommandSent,
        answered: LifecycleCommandSent,
    },
    #[error(
        "the helper process answered command {answered_command_id}, which the parent never sent \
         or already had the answer to, while it waited on its answer to {awaited}"
    )]
    AnsweredACommandNotAwaited {
        awaited: LifecycleCommandSent,
        answered_command_id: OutOfProcessCommandId,
    },
    #[error(
        "the helper process sent a reply naming no command while the parent waited on its \
         answer to {awaited}"
    )]
    ReplyNamedNoCommand { awaited: LifecycleCommandSent },
    #[error("the helper process was never sent `{}`", .command.lifecycle_command().command_tag())]
    ShutdownCommandNeverSent {
        command: HelperProcessShutdownCommand,
    },
}

/// The `stop` and `teardown` one helper was sent, each once it went out.
#[derive(Debug, Default)]
pub(crate) struct ShutdownCommandsSent {
    stop: Option<LifecycleCommandSent>,
    teardown: Option<LifecycleCommandSent>,
}

impl ShutdownCommandsSent {
    fn sent_for(&self, command: HelperProcessShutdownCommand) -> Option<LifecycleCommandSent> {
        match command {
            HelperProcessShutdownCommand::Stop => self.stop,
            HelperProcessShutdownCommand::Teardown => self.teardown,
        }
    }

    fn note_sent(&mut self, command: HelperProcessShutdownCommand, sent: LifecycleCommandSent) {
        match command {
            HelperProcessShutdownCommand::Stop => self.stop = Some(sent),
            HelperProcessShutdownCommand::Teardown => self.teardown = Some(sent),
        }
    }
}

/// One lifecycle command still owed its reply.
#[derive(Debug)]
struct ReplyOwedToASentCommand {
    command: LifecycleCommandAnsweredByAReply,
    /// Whether its waiter stopped waiting, so a reply that arrives late is
    /// dropped by name rather than read as the answer to another command.
    abandoned: bool,
    /// Its reply, read while another command's waiter waited, kept for its own
    /// waiter.
    arrived_out_of_step: Option<serde_json::Value>,
}

/// The lifecycle commands the parent writes to one helper, the replies it
/// still owes, and the queue its replies arrive on.
pub(super) struct LifecycleCommandsToOneSubprocess {
    parent_side: Arc<ParentSideOfOneSubprocessBridgeSharedByItsThreads>,
    replies: Receiver<serde_json::Value>,
    owed_by_command_id: parking_lot::Mutex<HashMap<OutOfProcessCommandId, ReplyOwedToASentCommand>>,
}

impl LifecycleCommandsToOneSubprocess {
    pub(super) fn over(
        parent_side: Arc<ParentSideOfOneSubprocessBridgeSharedByItsThreads>,
        replies: Receiver<serde_json::Value>,
    ) -> Self {
        Self {
            parent_side,
            replies,
            owed_by_command_id: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    /// Write a command the subprocess answers nothing to.
    pub(super) fn send_a_command_answered_by_nothing(&self, msg: &serde_json::Value) -> Result<()> {
        if self.parent_side.the_subprocess_was_given_up_on() {
            return Err(Error::Runtime(format!(
                "[{}] bridge marked dead, cannot send",
                self.parent_side.node_run_id
            )));
        }
        self.parent_side
            .write_frame_or_give_up_on_the_subprocess(msg)
    }

    /// Write `command` carrying `fields`, stamped with the id its reply is
    /// matched to.
    pub(super) fn send_a_lifecycle_command_awaiting_its_reply(
        &self,
        command: LifecycleCommandAnsweredByAReply,
        mut fields: LifecycleCommandFields,
    ) -> Result<LifecycleCommandSent> {
        let sent = LifecycleCommandSent {
            command,
            command_id: self.parent_side.mint_an_out_of_process_command_id(),
        };
        fields.insert("cmd".to_owned(), serde_json::json!(command.command_tag()));
        fields.insert(
            OUT_OF_PROCESS_COMMAND_ID_FIELD.to_owned(),
            serde_json::json!(sent.command_id),
        );
        // Owed before the write: the subprocess can answer the moment the
        // frame lands.
        self.owe_a_reply_to(sent);
        if let Err(not_sent) =
            self.send_a_command_answered_by_nothing(&serde_json::Value::Object(fields))
        {
            self.owed_by_command_id.lock().remove(&sent.command_id);
            return Err(not_sent);
        }
        Ok(sent)
    }

    /// Send `stop` then `teardown`, noting each in `shutdown_commands_sent` as
    /// it goes out, after abandoning every reply still owed: its waiter gave
    /// it up, as a `setup` cut short by shutdown was.
    pub(super) fn send_the_shutdown_commands(
        &self,
        shutdown_commands_sent: &mut ShutdownCommandsSent,
    ) -> Result<()> {
        self.owed_by_command_id.lock().retain(|_, owed| {
            owed.abandoned = true;
            owed.arrived_out_of_step.is_none()
        });
        for command in HelperProcessShutdownCommand::BOTH_IN_THE_ORDER_THE_LADDER_SENDS_THEM {
            let sent = self.send_a_lifecycle_command_awaiting_its_reply(
                command.lifecycle_command(),
                lifecycle_command_fields([("capability", serde_json::json!("full"))]),
            )?;
            shutdown_commands_sent.note_sent(command, sent);
        }
        Ok(())
    }

    /// Wait up to `slice` for the reply to `command`, as
    /// `shutdown_commands_sent` holds it. The ladder moves to `teardown`
    /// whether or not `stop` was answered, so waiting on `teardown` abandons
    /// the reply to `stop`.
    pub(super) fn await_the_reply_to_a_shutdown_command(
        &self,
        shutdown_commands_sent: &ShutdownCommandsSent,
        command: HelperProcessShutdownCommand,
        slice: Duration,
    ) -> std::result::Result<serde_json::Value, LifecycleReplyNotReceived> {
        if command == HelperProcessShutdownCommand::Teardown
            && let Some(stop_sent) = shutdown_commands_sent.stop
        {
            self.abandon_the_reply_to(stop_sent);
        }
        let awaited = shutdown_commands_sent
            .sent_for(command)
            .ok_or(LifecycleReplyNotReceived::ShutdownCommandNeverSent { command })?;
        self.await_the_reply_to(awaited, slice)
    }

    /// Owe `sent` its reply, before the command is written.
    fn owe_a_reply_to(&self, sent: LifecycleCommandSent) {
        self.owed_by_command_id.lock().insert(
            sent.command_id,
            ReplyOwedToASentCommand {
                command: sent.command,
                abandoned: false,
                arrived_out_of_step: None,
            },
        );
    }

    /// Wait up to `timeout` for the reply carrying `awaited`'s id.
    ///
    /// A late reply to a command whose waiter abandoned it is logged and
    /// passed over. Any other reply is an error naming both commands, since
    /// the helper answers each command in order; a reply to another command
    /// still awaited is kept for that command's own waiter.
    pub(super) fn await_the_reply_to(
        &self,
        awaited: LifecycleCommandSent,
        timeout: Duration,
    ) -> std::result::Result<serde_json::Value, LifecycleReplyNotReceived> {
        if let Some(reply) = self.take_the_reply_that_arrived_out_of_step_for(awaited) {
            return Ok(reply);
        }
        let deadline = Instant::now() + timeout;
        loop {
            let reply = match self
                .replies
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(reply) => reply,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(LifecycleReplyNotReceived::DeadlinePassed { awaited });
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(LifecycleReplyNotReceived::HelperProcessCanNoLongerAnswer {
                        awaited,
                    });
                }
            };
            let Some(answered_command_id) = out_of_process_command_id_of(&reply) else {
                return Err(LifecycleReplyNotReceived::ReplyNamedNoCommand { awaited });
            };
            let mut owed_by_command_id = self.owed_by_command_id.lock();
            let owed = owed_by_command_id.remove(&answered_command_id);
            if answered_command_id == awaited.command_id {
                return Ok(reply);
            }
            let Some(mut owed) = owed else {
                return Err(LifecycleReplyNotReceived::AnsweredACommandNotAwaited {
                    awaited,
                    answered_command_id,
                });
            };
            let answered = LifecycleCommandSent {
                command: owed.command,
                command_id: answered_command_id,
            };
            if !owed.abandoned {
                owed.arrived_out_of_step = Some(reply);
                owed_by_command_id.insert(answered_command_id, owed);
                return Err(LifecycleReplyNotReceived::AnsweredAnotherCommand {
                    awaited,
                    answered,
                });
            }
            tracing::debug!(
                "[{}] its helper process answered {answered} after the parent stopped waiting on \
                 it; dropped",
                self.parent_side.node_run_id
            );
        }
    }

    /// Stop waiting on the reply to `sent`, so one that arrives later is
    /// dropped rather than read as the answer to another command.
    pub(super) fn abandon_the_reply_to(&self, sent: LifecycleCommandSent) {
        let mut owed_by_command_id = self.owed_by_command_id.lock();
        match owed_by_command_id.get_mut(&sent.command_id) {
            Some(owed) if owed.arrived_out_of_step.is_some() => {
                owed_by_command_id.remove(&sent.command_id);
            }
            Some(owed) => owed.abandoned = true,
            None => {}
        }
    }

    /// The reply to `awaited` read while another command's waiter waited.
    fn take_the_reply_that_arrived_out_of_step_for(
        &self,
        awaited: LifecycleCommandSent,
    ) -> Option<serde_json::Value> {
        let mut owed_by_command_id = self.owed_by_command_id.lock();
        let Entry::Occupied(owed) = owed_by_command_id.entry(awaited.command_id) else {
            return None;
        };
        owed.get().arrived_out_of_step.as_ref()?;
        owed.remove().arrived_out_of_step
    }
}

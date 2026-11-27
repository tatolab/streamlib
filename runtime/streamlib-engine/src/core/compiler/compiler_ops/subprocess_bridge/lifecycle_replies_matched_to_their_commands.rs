// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The replies a helper sends to the lifecycle commands its parent waits on,
//! each matched to its command by the [`OutOfProcessCommandId`] it echoes.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::core::nodes::OutOfProcessCommandId;

use super::out_of_process_command_id_of;

/// A lifecycle command the parent sends a helper and waits on the reply to.
///
/// `run`, `unwire_link`, `unfeed_island` and `set_input_values` are absent:
/// they are answered by nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LifecycleCommandAwaitingItsReply {
    Setup,
    Stop,
    Teardown,
    OnPause,
    OnResume,
    CarryLinks,
}

impl LifecycleCommandAwaitingItsReply {
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

/// One lifecycle command sent, by the id its reply is matched to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LifecycleCommandSent {
    pub(crate) command: LifecycleCommandAwaitingItsReply,
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
}

/// One lifecycle command still owed its reply.
#[derive(Clone, Copy, Debug)]
struct LifecycleCommandOwedItsReply {
    command: LifecycleCommandAwaitingItsReply,
    /// Whether its waiter stopped waiting, so a reply that arrives late is
    /// dropped by name rather than read as the answer to another command.
    abandoned: bool,
}

/// The lifecycle commands one helper still owes a reply to, and the queue its
/// replies arrive on.
pub(super) struct LifecycleRepliesMatchedToTheirCommands {
    replies: Receiver<serde_json::Value>,
    owed_by_command_id:
        parking_lot::Mutex<HashMap<OutOfProcessCommandId, LifecycleCommandOwedItsReply>>,
}

impl LifecycleRepliesMatchedToTheirCommands {
    pub(super) fn arriving_on(replies: Receiver<serde_json::Value>) -> Self {
        Self {
            replies,
            owed_by_command_id: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    /// Owe `sent` its reply, before the command is written.
    pub(super) fn owe_a_reply_to(&self, sent: LifecycleCommandSent) {
        self.owed_by_command_id.lock().insert(
            sent.command_id,
            LifecycleCommandOwedItsReply {
                command: sent.command,
                abandoned: false,
            },
        );
    }

    /// Owe `sent` nothing, because it was never written.
    pub(super) fn owe_no_reply_to(&self, sent: LifecycleCommandSent) {
        self.owed_by_command_id.lock().remove(&sent.command_id);
    }

    /// Wait up to `timeout` for the reply carrying `awaited`'s id.
    ///
    /// A late reply to a command whose waiter abandoned it is logged and
    /// passed over. Any other reply is an error naming both commands: the
    /// helper answers each command it is sent, in order, so it means the two
    /// sides no longer agree on what was asked.
    pub(super) fn await_the_reply_to(
        &self,
        awaited: LifecycleCommandSent,
        timeout: Duration,
        node_run_id: &str,
    ) -> std::result::Result<serde_json::Value, LifecycleReplyNotReceived> {
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
            let owed = self.owed_by_command_id.lock().remove(&answered_command_id);
            if answered_command_id == awaited.command_id {
                return Ok(reply);
            }
            match owed {
                Some(LifecycleCommandOwedItsReply {
                    command,
                    abandoned: true,
                }) => {
                    tracing::debug!(
                        "[{node_run_id}] its helper process answered {} after the parent \
                         stopped waiting on it; dropped",
                        LifecycleCommandSent {
                            command,
                            command_id: answered_command_id,
                        },
                    );
                }
                Some(LifecycleCommandOwedItsReply {
                    command,
                    abandoned: false,
                }) => {
                    return Err(LifecycleReplyNotReceived::AnsweredAnotherCommand {
                        awaited,
                        answered: LifecycleCommandSent {
                            command,
                            command_id: answered_command_id,
                        },
                    });
                }
                None => {
                    return Err(LifecycleReplyNotReceived::AnsweredACommandNotAwaited {
                        awaited,
                        answered_command_id,
                    });
                }
            }
        }
    }

    /// Stop waiting on the reply to `sent`, so one that arrives later is
    /// dropped rather than read as the answer to another command.
    pub(super) fn abandon_the_reply_to(&self, sent: LifecycleCommandSent) {
        if let Some(owed) = self.owed_by_command_id.lock().get_mut(&sent.command_id) {
            owed.abandoned = true;
        }
    }

    /// Stop waiting on the reply to every command still owed one.
    pub(super) fn abandon_the_reply_to_every_command_still_owed_one(&self) {
        for owed in self.owed_by_command_id.lock().values_mut() {
            owed.abandoned = true;
        }
    }
}

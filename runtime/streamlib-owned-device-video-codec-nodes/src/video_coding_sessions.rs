// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The sessions a codec node's thread codes with. A session is opened on the coding thread
//! beside the ones already open, at the request of a build of the node's settings, and closed
//! when that build's value is let go; an encoder codes with the newest one, and a decoder moves
//! to the newest one at a group start, so the stream never stops for a new setting.

use std::fmt;
use std::sync::mpsc;
use std::time::Duration;

/// Why a session a build asked for did not open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoCodingSessionNotOpened {
    /// The codec refused it: its settings, the device, or the video codec host. The sessions
    /// already open keep coding.
    Refused(String),
    /// The coding thread has ended, so nothing opens on it.
    CodingThreadEnded { node_type_name: &'static str },
    /// The coding thread did not answer within `waited`; the session is closed if it opens
    /// after.
    Unanswered {
        node_type_name: &'static str,
        waited: Duration,
    },
}

impl fmt::Display for VideoCodingSessionNotOpened {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(refusal) => formatter.write_str(refusal),
            Self::CodingThreadEnded { node_type_name } => write!(
                formatter,
                "{node_type_name}: the coding thread has ended, so no session opens on it"
            ),
            Self::Unanswered {
                node_type_name,
                waited,
            } => write!(
                formatter,
                "{node_type_name}: the coding thread did not open the session within {waited:?}"
            ),
        }
    }
}

impl std::error::Error for VideoCodingSessionNotOpened {}

/// What a coding thread is asked to do with its sessions.
pub(crate) enum VideoCodingSessionCommand<Settings> {
    /// Open a session of `settings` numbered `number`, and answer whether it opened.
    Open {
        number: u64,
        settings: Settings,
        answer: mpsc::SyncSender<Result<(), VideoCodingSessionNotOpened>>,
    },
    /// Close the session numbered `number`.
    Close { number: u64 },
}

/// The sessions open on one coding thread, oldest first, each by the number it was opened
/// under.
pub(crate) struct VideoCodingSessionsOfTheThread<Session> {
    open: Vec<(u64, Session)>,
}

impl<Session> Default for VideoCodingSessionsOfTheThread<Session> {
    fn default() -> Self {
        Self { open: Vec::new() }
    }
}

impl<Session> VideoCodingSessionsOfTheThread<Session> {
    /// Carry out every command waiting in `commands`, in the order they were sent: each open
    /// with `open`, which is told whether another session is open, and answered; each close by
    /// dropping its session here, on the coding thread.
    pub(crate) fn take_the_commands<Settings>(
        &mut self,
        commands: &mpsc::Receiver<VideoCodingSessionCommand<Settings>>,
        mut open: impl FnMut(Settings, bool) -> Result<Session, VideoCodingSessionNotOpened>,
    ) {
        while let Ok(command) = commands.try_recv() {
            match command {
                VideoCodingSessionCommand::Open {
                    number,
                    settings,
                    answer,
                } => {
                    let opened = open(settings, !self.open.is_empty());
                    let answered = match opened {
                        Ok(session) => {
                            self.open.push((number, session));
                            Ok(())
                        }
                        Err(refusal) => Err(refusal),
                    };
                    // A build that stopped waiting closes what it asked for, so its close
                    // follows in `commands`.
                    let _ = answer.send(answered);
                }
                VideoCodingSessionCommand::Close { number } => {
                    self.open.retain(|(open_number, _)| *open_number != number);
                }
            }
        }
    }

    /// The newest session open, which an encoder codes every frame with.
    pub(crate) fn newest_mut(&mut self) -> Option<&mut Session> {
        self.open.last_mut().map(|(_, session)| session)
    }

    /// Whether a session numbered `number` is open.
    pub(crate) fn is_open(&self, number: u64) -> bool {
        self.open
            .iter()
            .any(|(open_number, _)| *open_number == number)
    }

    /// The session a decoder decodes the next media frame with, `decoding_with` naming the one
    /// the last media frame went to and updated when it moves.
    ///
    /// It stays with that session until a newer one can take a group start: the newest session
    /// for which `takes_frames` answers true then takes it, so a new session enters the stream
    /// where a decoder may. With no session decoding, for the first frame or once that session
    /// closed, the newest that takes frames takes any media frame, and the decoder's own entry
    /// gate waits for the group start.
    pub(crate) fn the_session_decoding_a_media_frame(
        &mut self,
        decoding_with: &mut Option<u64>,
        starts_group: bool,
        mut takes_frames: impl FnMut(&mut Session) -> bool,
    ) -> Option<&mut Session> {
        if let Some(number) = *decoding_with
            && !self.is_open(number)
        {
            *decoding_with = None;
        }
        let may_move = starts_group || decoding_with.is_none();
        let mut chosen = None;
        for index in (0..self.open.len()).rev() {
            let (number, session) = &mut self.open[index];
            if Some(*number) == *decoding_with {
                chosen = Some(index);
                break;
            }
            if may_move && takes_frames(session) {
                *decoding_with = Some(*number);
                chosen = Some(index);
                break;
            }
        }
        chosen.map(|index| &mut self.open[index].1)
    }
}

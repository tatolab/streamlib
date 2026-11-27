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
    /// The GPU lacked something another of its clients may free, such as a video session, while
    /// another session of the node decodes; a later try may open it. The sessions already open
    /// keep coding.
    RefusedForNow(String),
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
            Self::Refused(refusal) | Self::RefusedForNow(refusal) => formatter.write_str(refusal),
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
/// under, and which one the last media frame was decoded with.
pub(crate) struct VideoCodingSessionsOfTheThread<Session> {
    open: Vec<SessionOfTheThread<Session>>,
    decoding_with: Option<u64>,
}

struct SessionOfTheThread<Session> {
    number: u64,
    session: Session,
    /// Its build let it go while it decoded and a newer session had yet to take a group start,
    /// so it decodes on until one does.
    let_go: bool,
}

impl<Session> Default for VideoCodingSessionsOfTheThread<Session> {
    fn default() -> Self {
        Self {
            open: Vec::new(),
            decoding_with: None,
        }
    }
}

impl<Session> VideoCodingSessionsOfTheThread<Session> {
    /// Carry out every command waiting in `commands`, in the order they were sent: each open
    /// with `open`, which is told whether another session already codes (one `codes` answers
    /// true for), and answered; each close by dropping its session here, on the coding thread.
    ///
    /// A decoder's session let go before a newer one has taken a group start decodes on until
    /// one does, since a node's build lets the old value go as soon as the new one is built.
    pub(crate) fn take_the_commands<Settings>(
        &mut self,
        commands: &mpsc::Receiver<VideoCodingSessionCommand<Settings>>,
        codes: impl Fn(&Session) -> bool,
        mut open: impl FnMut(Settings, bool) -> Result<Session, VideoCodingSessionNotOpened>,
    ) {
        while let Ok(command) = commands.try_recv() {
            match command {
                VideoCodingSessionCommand::Open {
                    number,
                    settings,
                    answer,
                } => {
                    let another_codes = self.open.iter().any(|open| codes(&open.session));
                    let answered = open(settings, another_codes).map(|session| {
                        self.open.push(SessionOfTheThread {
                            number,
                            session,
                            let_go: false,
                        });
                    });
                    // A build that stopped waiting closes what it asked for, so its close
                    // follows in `commands`.
                    let _ = answer.send(answered);
                }
                VideoCodingSessionCommand::Close { number } => {
                    let Some(index) = self.open.iter().position(|open| open.number == number)
                    else {
                        continue;
                    };
                    let a_newer_one_waits = self.open[index + 1..].iter().any(|open| !open.let_go);
                    if self.decoding_with == Some(number) && a_newer_one_waits {
                        self.open[index].let_go = true;
                    } else {
                        self.open.remove(index);
                    }
                }
            }
            self.drop_what_was_let_go_with_none_newer_waiting();
        }
    }

    fn drop_what_was_let_go_with_none_newer_waiting(&mut self) {
        while let Some(index) = self.open.iter().rposition(|open| open.let_go)
            && !self.open[index + 1..].iter().any(|open| !open.let_go)
        {
            self.open.remove(index);
        }
    }

    /// The newest session open, which an encoder codes every frame with.
    pub(crate) fn newest_mut(&mut self) -> Option<&mut Session> {
        self.open.last_mut().map(|open| &mut open.session)
    }

    /// Whether a session numbered `number` is open, let go or not.
    pub(crate) fn is_open(&self, number: u64) -> bool {
        self.open.iter().any(|open| open.number == number)
    }

    /// The session a decoder decodes the next media frame with.
    ///
    /// It stays with the session the last media frame went to until a newer one can take a
    /// group start: the newest session for which `takes_frames` answers true then takes it, so a
    /// new session enters the stream where a decoder may, and the sessions let go before it are
    /// dropped. With no session decoding, for the first frame or once that session closed, the
    /// newest that takes frames takes any media frame, and the decoder's own entry gate waits
    /// for the group start.
    pub(crate) fn the_session_decoding_a_media_frame(
        &mut self,
        starts_group: bool,
        mut takes_frames: impl FnMut(&mut Session) -> bool,
    ) -> Option<&mut Session> {
        if let Some(number) = self.decoding_with
            && !self.is_open(number)
        {
            self.decoding_with = None;
        }
        let may_move = starts_group || self.decoding_with.is_none();
        let decoding_with = self.decoding_with;
        let index = self.open.iter_mut().rposition(|open| {
            Some(open.number) == decoding_with
                || (may_move && !open.let_go && takes_frames(&mut open.session))
        })?;
        let number = self.open[index].number;
        if Some(number) != decoding_with {
            self.decoding_with = Some(number);
            let dropped_before = self.open[..index].iter().filter(|open| open.let_go).count();
            self.open.retain(|open| !open.let_go);
            return self
                .open
                .get_mut(index - dropped_before)
                .map(|open| &mut open.session);
        }
        self.open.get_mut(index).map(|open| &mut open.session)
    }
}

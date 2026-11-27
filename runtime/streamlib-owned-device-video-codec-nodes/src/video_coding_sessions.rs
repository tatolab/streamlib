// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The sessions a codec node's thread codes with. A session is opened on the coding thread
//! beside the ones already open, at the request of a build of the node's settings, and closed
//! when that build's value is let go; an encoder codes with the newest one, and a decoder moves
//! to the newest one at a group start, so the stream never stops for a new setting.

use std::fmt;
use std::sync::mpsc;
use std::time::{Duration, Instant};

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
/// under, which one the last frame was coded with, and whether a build has asked for a session
/// yet.
pub(crate) struct VideoCodingSessionsOfTheThread<Session> {
    open: Vec<SessionOfTheThread<Session>>,
    coding_with: Option<u64>,
    a_session_was_asked_for: bool,
}

struct SessionOfTheThread<Session> {
    number: u64,
    session: Session,
    /// Its build let it go while it coded and a newer session had yet to take over, so it
    /// codes on until one does.
    let_go: bool,
    /// When an encoder tries this session again, after it refused a frame beside an older one
    /// that encodes.
    next_try: Option<Instant>,
}

/// How the sessions took one frame to encode.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct FrameEncodedWithTheSessions<Refusal> {
    /// What the session that took the frame answered; `None` with no session open.
    pub(crate) taken: Option<Result<(), Refusal>>,
    /// The newest session's refusal, when it was tried, refused, and an older one took the frame.
    pub(crate) newest_refused: Option<Refusal>,
}

impl<Session> Default for VideoCodingSessionsOfTheThread<Session> {
    fn default() -> Self {
        Self {
            open: Vec::new(),
            coding_with: None,
            a_session_was_asked_for: false,
        }
    }
}

impl<Session> VideoCodingSessionsOfTheThread<Session> {
    /// Carry out every command waiting in `commands`, in the order they were sent: each open
    /// with `open`, which is told whether another session already codes (one `codes` answers
    /// true for), and answered; each close by dropping its session here, on the coding thread.
    /// Until a build has asked for a session, it waits up to `wait_for_the_first` for one, so
    /// the thread can hold its input unread until the node's first build has run.
    ///
    /// A session let go while it codes, before a newer one has taken over, codes on until one
    /// does, since a node's build lets the old value go as soon as the new one is built.
    pub(crate) fn take_the_commands<Settings>(
        &mut self,
        commands: &mpsc::Receiver<VideoCodingSessionCommand<Settings>>,
        wait_for_the_first: Duration,
        codes: impl Fn(&Session) -> bool,
        mut open: impl FnMut(Settings, bool) -> Result<Session, VideoCodingSessionNotOpened>,
    ) {
        let first = if self.a_session_was_asked_for {
            None
        } else {
            commands.recv_timeout(wait_for_the_first).ok()
        };
        for command in first
            .into_iter()
            .chain(std::iter::from_fn(|| commands.try_recv().ok()))
        {
            match command {
                VideoCodingSessionCommand::Open {
                    number,
                    settings,
                    answer,
                } => {
                    self.a_session_was_asked_for = true;
                    let another_codes = self.open.iter().any(|open| codes(&open.session));
                    let answered = open(settings, another_codes).map(|session| {
                        self.open.push(SessionOfTheThread {
                            number,
                            session,
                            let_go: false,
                            next_try: None,
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
                    if self.coding_with == Some(number) && a_newer_one_waits {
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

    /// Whether a build has asked for a session, opened or refused; until one has, the thread
    /// leaves its input unread.
    pub(crate) fn a_session_was_asked_for(&self) -> bool {
        self.a_session_was_asked_for
    }

    /// The newest session open.
    #[cfg(test)]
    pub(crate) fn newest_mut(&mut self) -> Option<&mut Session> {
        self.open.last_mut().map(|open| &mut open.session)
    }

    /// Encode one frame at `now` with `encode`.
    ///
    /// The newest session takes it once it has encoded a frame. Until then, while an older
    /// session encoded the last frame, the newest is tried at most once every `retry_interval`,
    /// and the older one encodes each frame the newest does not, so a new session that refuses
    /// to encode, its settings beyond what the device does say, leaves the stream encoding as
    /// before. Once the newest encodes a frame, the sessions let go before it are dropped.
    pub(crate) fn encode_a_frame<Refusal>(
        &mut self,
        now: Instant,
        retry_interval: Duration,
        mut encode: impl FnMut(&mut Session) -> Result<(), Refusal>,
    ) -> FrameEncodedWithTheSessions<Refusal> {
        let Some(newest) = self.open.len().checked_sub(1) else {
            return FrameEncodedWithTheSessions {
                taken: None,
                newest_refused: None,
            };
        };
        let older_encoding = self
            .coding_with
            .and_then(|number| self.open.iter().position(|open| open.number == number))
            .filter(|&index| index != newest);
        let Some(older_encoding) = older_encoding else {
            let taken = encode(&mut self.open[newest].session);
            if taken.is_ok() {
                self.take_over_with_the_newest();
            }
            return FrameEncodedWithTheSessions {
                taken: Some(taken),
                newest_refused: None,
            };
        };
        let mut newest_refused = None;
        if self.open[newest]
            .next_try
            .is_none_or(|next_try| now >= next_try)
        {
            match encode(&mut self.open[newest].session) {
                Ok(()) => {
                    self.take_over_with_the_newest();
                    return FrameEncodedWithTheSessions {
                        taken: Some(Ok(())),
                        newest_refused: None,
                    };
                }
                Err(refusal) => {
                    self.open[newest].next_try = Some(now + retry_interval);
                    newest_refused = Some(refusal);
                }
            }
        }
        FrameEncodedWithTheSessions {
            taken: Some(encode(&mut self.open[older_encoding].session)),
            newest_refused,
        }
    }

    fn take_over_with_the_newest(&mut self) {
        self.open.retain(|open| !open.let_go);
        self.coding_with = self.open.last().map(|open| open.number);
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
        if let Some(number) = self.coding_with
            && !self.is_open(number)
        {
            self.coding_with = None;
        }
        let may_move = starts_group || self.coding_with.is_none();
        let coding_with = self.coding_with;
        let index = self.open.iter_mut().rposition(|open| {
            Some(open.number) == coding_with
                || (may_move && !open.let_go && takes_frames(&mut open.session))
        })?;
        let number = self.open[index].number;
        if Some(number) != coding_with {
            self.coding_with = Some(number);
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

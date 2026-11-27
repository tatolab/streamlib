// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The calls too slow to run every frame: creating a kernel, opening a device and starting a
//! session. Each is refused while a frame runs on the calling thread, and runs in the node's body
//! and in a build.

use std::fmt;

use crate::{READING, Reading};

/// What a call too slow to run every frame does, each refused by its own code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallTooSlowForAFrame {
    /// The call creates a kernel.
    CreatesAKernel,
    /// The call opens a device.
    OpensADevice,
    /// The call starts a session.
    StartsASession,
}

impl CallTooSlowForAFrame {
    /// `call`, which does this, refused while a frame runs on this thread; `Ok` in the node's
    /// body, in a build, and anywhere else.
    pub fn refused_while_a_frame_runs(self, call: &str) -> Result<(), CallRefusedInAFrame> {
        let a_frame_runs =
            READING.with(|reading| matches!(&*reading.borrow(), Some(Reading::Frame { .. })));
        if a_frame_runs {
            Err(CallRefusedInAFrame {
                does: self,
                call: call.to_owned(),
            })
        } else {
            Ok(())
        }
    }

    /// The code the refusal is known by: `kernel_created_in_a_frame`, `device_opened_in_a_frame`
    /// or `session_started_in_a_frame`.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            CallTooSlowForAFrame::CreatesAKernel => "kernel_created_in_a_frame",
            CallTooSlowForAFrame::OpensADevice => "device_opened_in_a_frame",
            CallTooSlowForAFrame::StartsASession => "session_started_in_a_frame",
        }
    }

    /// What the call makes, as the name of the computed value that holds it: `kernel`, `device`
    /// or `session`.
    #[must_use]
    pub fn what_it_makes(self) -> &'static str {
        match self {
            CallTooSlowForAFrame::CreatesAKernel => "kernel",
            CallTooSlowForAFrame::OpensADevice => "device",
            CallTooSlowForAFrame::StartsASession => "session",
        }
    }

    /// What the call does: `creates a kernel`, `opens a device` or `starts a session`.
    #[must_use]
    pub fn what_it_does(self) -> &'static str {
        match self {
            CallTooSlowForAFrame::CreatesAKernel => "creates a kernel",
            CallTooSlowForAFrame::OpensADevice => "opens a device",
            CallTooSlowForAFrame::StartsASession => "starts a session",
        }
    }
}

/// A call too slow to run every frame, refused because a frame runs on this thread: what it does
/// and the name its caller calls it by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallRefusedInAFrame {
    pub does: CallTooSlowForAFrame,
    pub call: String,
}

impl CallRefusedInAFrame {
    /// The refusal and its fix, with `wrapped_call`, the call wrapped in a computed value as the
    /// node's language writes it, as the fix.
    #[must_use]
    pub fn words_with_the_fix(&self, wrapped_call: &str) -> String {
        format!(
            "`{}` {} and can't run every frame; wrap it in a computed value in the node's body: \
             `{wrapped_call}`, then read `{}.get()` in the per-frame function",
            self.call,
            self.does.what_it_does(),
            self.does.what_it_makes()
        )
    }
}

/// The refusal with its fix written in Rust; a binding writes the fix in its own language.
impl fmt::Display for CallRefusedInAFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.words_with_the_fix(&format!(
            "let {} = computed(move || … {}(…));",
            self.does.what_it_makes(),
            self.call
        )))
    }
}

impl std::error::Error for CallRefusedInAFrame {}

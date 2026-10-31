// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/// Whether opening a privacy-gated capture device may put the platform's
/// access prompt in front of the user when nobody has answered it yet.
///
/// A prompt answers something the user did: a stream they loaded or started
/// may ask, and a stream the runtime re-loaded by itself at its start may not,
/// so the user is never asked out of nowhere. An access already granted or
/// refused is honoured either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaptureDeviceAccessPrompting {
    /// Someone loaded or started the stream, so the platform may ask them.
    #[default]
    MayAskTheUser,
    /// Nobody acted on the stream, so an access never asked for is refused by
    /// name, naming what to do to be asked, instead of prompting.
    RefuseWhatWasNeverAskedFor,
}

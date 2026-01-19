// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Built-in schemas for StreamLib IPC communication.
//!
//! These types are MessagePack-serializable for cross-process communication
//! via iceoryx2.

mod com_tatolab_audioframe_1ch;
mod com_tatolab_audioframe_2ch;
mod com_tatolab_audioframe_3ch;
mod com_tatolab_audioframe_4ch;
mod com_tatolab_audioframe_5ch;
mod com_tatolab_audioframe_6ch;
mod com_tatolab_audioframe_7ch;
mod com_tatolab_audioframe_8ch;
mod com_tatolab_videoframe;

pub use com_tatolab_audioframe_1ch::Audioframe1ch;
pub use com_tatolab_audioframe_2ch::Audioframe2ch;
pub use com_tatolab_audioframe_3ch::Audioframe3ch;
pub use com_tatolab_audioframe_4ch::Audioframe4ch;
pub use com_tatolab_audioframe_5ch::Audioframe5ch;
pub use com_tatolab_audioframe_6ch::Audioframe6ch;
pub use com_tatolab_audioframe_7ch::Audioframe7ch;
pub use com_tatolab_audioframe_8ch::Audioframe8ch;
pub use com_tatolab_videoframe::Videoframe;

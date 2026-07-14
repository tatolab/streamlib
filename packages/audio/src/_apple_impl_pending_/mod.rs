// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Parked cross-platform Apple-flavored audio codec types (carved out
//! of `runtime/streamlib-engine/src/core/codec/audio_codec.rs` in #786).
//! Gated so it never compiles; re-enable + rewire imports once Apple
//! support is activated for `@tatolab/audio`.

pub mod audio_codec;

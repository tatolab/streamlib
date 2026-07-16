// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cross-submodule helpers used by every host-side vtable callback.

pub(in crate::core::plugin::host_services) mod wire;

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) mod borrow;

// Shared `Repr <-> engine` conversions for the hardware video surface
// (#1259). Linux-only: the engine codec types they map to live in the
// `#[cfg(target_os = "linux")]` `vulkan::video` module. The encoder
// fill-in (#1376) lands them; the decoder sibling (#1377) imports them.
#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) mod video_codec_repr;

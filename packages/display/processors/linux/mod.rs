// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![cfg(target_os = "linux")]

pub mod display;

pub use display::{LinuxDisplayProcessor, LinuxWindowId};

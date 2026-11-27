// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! An audio stream one node's module opened on a device, which a later open of the same device by
//! that node takes over: the earlier stream stops and gives the device back before the later one
//! opens. So a computed value opening a device in place of the value it replaces never holds the
//! device beside it, and an exclusive device is never refused as busy by its own node.

use std::sync::{Arc, Mutex};

use super::audio_capture_streams::locked;

/// The stream a module opened on one device, until a later open of that device takes it over.
pub(super) struct AudioStreamOnADevice<S: ?Sized> {
    /// `None` for the backend's default device.
    device_id: Option<String>,
    stream: Mutex<Option<Box<S>>>,
}

impl<S: ?Sized> AudioStreamOnADevice<S> {
    pub(super) fn opened_on(device_id: Option<String>, stream: Box<S>) -> Self {
        Self {
            device_id,
            stream: Mutex::new(Some(stream)),
        }
    }

    /// Whether the stream was opened on `device_id`, the default device's being `None`.
    pub(super) fn is_on(&self, device_id: Option<&str>) -> bool {
        self.device_id.as_deref() == device_id
    }

    /// Whether a later open took the stream's device over.
    pub(super) fn is_taken_over(&self) -> bool {
        locked(&self.stream).is_none()
    }

    /// Run `with` on the stream, or answer `None` once a later open took its device over.
    pub(super) fn while_it_holds_its_device<T>(&self, with: impl FnOnce(&mut S) -> T) -> Option<T> {
        locked(&self.stream).as_deref_mut().map(with)
    }

    /// Stop the stream with `stop` and give its device back; a stream taken over already stays
    /// as it is.
    pub(super) fn give_its_device_up(&self, stop: impl FnOnce(&mut S)) {
        let given_up = locked(&self.stream).take();
        if let Some(mut stream) = given_up {
            stop(&mut *stream);
        }
    }
}

/// Take `device_id` over from each stream in `opened` on it, stopping each with `stop`; the
/// streams are collected first so no stop runs with `opened` locked.
pub(super) fn take_the_device_over_from<O, S: ?Sized>(
    opened: &Mutex<Vec<Option<Arc<O>>>>,
    device_id: Option<&str>,
    stream_of: impl Fn(&O) -> &AudioStreamOnADevice<S>,
    stop: impl Fn(&mut S),
) {
    let taken_over: Vec<Arc<O>> = locked(opened)
        .iter()
        .flatten()
        .filter(|opened| stream_of(opened).is_on(device_id))
        .cloned()
        .collect();
    for opened in taken_over {
        stream_of(&opened).give_its_device_up(&stop);
    }
}

/// Why a stream whose device a later open took over refuses to start.
pub(super) fn refusal_of_a_stream_taken_over(direction: &str) -> String {
    format!(
        "this audio {direction} stream's device was taken over by a later open of it by the same \
         node: open the device again to use it"
    )
}

#[cfg(test)]
mod tests;

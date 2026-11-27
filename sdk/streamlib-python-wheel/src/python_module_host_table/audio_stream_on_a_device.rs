// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! An audio stream one node's module opened on a device, which a later open of the same device by
//! that node takes over: the earlier stream stops and gives the device back before the later one
//! opens. So a computed value opening a device in place of the value it replaces never holds the
//! device beside it, and an exclusive device is never refused as busy by its own node.

use std::sync::Mutex;

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

/// Why a stream whose device a later open took over refuses to start.
pub(super) fn refusal_of_a_stream_taken_over(direction: &str) -> String {
    format!(
        "this audio {direction} stream's device was taken over by a later open of it by the same \
         node: open the device again to use it"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stream_given_up_stops_once_and_runs_nothing_after() {
        let on_a_device = AudioStreamOnADevice::opened_on(Some("hw:1".to_owned()), Box::new(0_u32));

        on_a_device.give_its_device_up(|stops| *stops += 1);
        on_a_device.give_its_device_up(|_| panic!("a stream given up stops once"));

        assert_eq!(on_a_device.while_it_holds_its_device(|_| ()), None);
    }

    #[test]
    fn a_stream_is_on_the_device_it_was_opened_on_and_on_no_other() {
        let on_the_default = AudioStreamOnADevice::opened_on(None, Box::new(()));
        let on_a_named_one = AudioStreamOnADevice::opened_on(Some("hw:1".to_owned()), Box::new(()));

        assert!(on_the_default.is_on(None));
        assert!(!on_the_default.is_on(Some("hw:1")));
        assert!(on_a_named_one.is_on(Some("hw:1")));
        assert!(!on_a_named_one.is_on(None));
    }
}

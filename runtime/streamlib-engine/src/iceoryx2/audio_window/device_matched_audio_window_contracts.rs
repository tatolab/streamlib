// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What settles an input port's `match_device` declaration, and where the
//! settled values live.
//!
//! The sentinel names a contract only the declaring processor can state: its
//! five values come from the format of the device stream that processor opened
//! in `setup()`, which varies by machine. So the values arrive after the port
//! was declared and — because the compiler wires every link before it releases
//! any processor into `setup()` — after that port was wired too. This module is
//! the one place they land, read by the wiring path when a link is added later,
//! by the port's own mailbox when the stage is installed, and by `graph`, which
//! renders what was resolved rather than the sentinel that asked for it.

use std::collections::HashMap;

use parking_lot::Mutex;

use super::ResolvedAudioWindowContract;
use crate::core::context::AudioStreamFormat;
use crate::core::descriptors::AudioWindowContractDeclaredValues;

/// The window contract a processor states for one of its own input ports from
/// the device stream it just opened.
///
/// The format is passed whole rather than as three loose values: it is exactly
/// what [`AudioPlaybackStream::stream_format`] hands back, so a caller cannot
/// mis-map a scalar encoding on the way in.
///
/// [`AudioPlaybackStream::stream_format`]: crate::core::context::AudioPlaybackStream::stream_format
#[derive(Debug, Clone, Copy)]
pub struct AudioWindowContractMatchingADeviceStream {
    /// The rate, channel count and scalar encoding the device stream opened at
    /// — what every block reaching the port is converted into.
    pub device_stream_format: AudioStreamFormat,
    /// Per-channel samples in one emitted window.
    pub window_size_in_per_channel_samples: u32,
    /// Per-channel samples between the starts of consecutive windows. Equal to
    /// the window size for contiguous windows, below it for a rolling one.
    pub hop_in_per_channel_samples: u32,
}

/// One processor's settled `match_device` contracts, keyed by the input port
/// each was settled for.
///
/// Held behind an `Arc` shared by the processor's input mailboxes and its graph
/// node, the way the per-inbound-link drop counts already are: a snapshot reads
/// what stands rather than a copy taken at wiring time, and no reader has to
/// lock a running processor to see it.
#[derive(Debug, Default)]
pub struct DeviceMatchedAudioWindowContractsByInputPort {
    settled: Mutex<HashMap<String, ResolvedAudioWindowContract>>,
}

impl DeviceMatchedAudioWindowContractsByInputPort {
    /// Record the contract `port_name` resolved to, replacing any earlier one.
    ///
    /// Replacing rather than refusing a second call: a processor stopped and
    /// started again opens its device afresh, and the format it gets is the
    /// machine's answer that time, not the previous one.
    pub(crate) fn settle_for_input_port(
        &self,
        port_name: &str,
        contract: ResolvedAudioWindowContract,
    ) {
        self.settled.lock().insert(port_name.to_string(), contract);
    }

    /// The contract `port_name` resolved to, or `None` while nothing has
    /// resolved one.
    pub(crate) fn settled_for_input_port(
        &self,
        port_name: &str,
    ) -> Option<ResolvedAudioWindowContract> {
        self.settled.lock().get(port_name).copied()
    }

    /// The contract `port_name` resolved to, as the five values a declaration
    /// states — what `graph` renders in place of the sentinel.
    pub fn settled_declaration_for_input_port(
        &self,
        port_name: &str,
    ) -> Option<AudioWindowContractDeclaredValues> {
        self.settled_for_input_port(port_name)
            .map(|contract| contract.as_declared_values())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::context::AudioSampleFormat;

    fn a_device_stream_format(sample_rate: u32, channels: u32) -> AudioStreamFormat {
        AudioStreamFormat {
            sample_rate,
            channels,
            sample_format: AudioSampleFormat::F32,
        }
    }

    #[test]
    fn a_port_nothing_resolved_reads_back_as_unsettled() {
        let contracts = DeviceMatchedAudioWindowContractsByInputPort::default();

        assert!(contracts.settled_for_input_port("audio").is_none());
        assert!(
            contracts
                .settled_declaration_for_input_port("audio")
                .is_none()
        );
    }

    #[test]
    fn a_settled_port_renders_the_five_values_the_device_gave_it() {
        let contracts = DeviceMatchedAudioWindowContractsByInputPort::default();
        let resolved = ResolvedAudioWindowContract::from_a_device_stream_format(
            &AudioWindowContractMatchingADeviceStream {
                device_stream_format: a_device_stream_format(48_000, 2),
                window_size_in_per_channel_samples: 512,
                hop_in_per_channel_samples: 512,
            },
        )
        .expect("a device format resolves");

        contracts.settle_for_input_port("audio", resolved);

        let declaration = contracts
            .settled_declaration_for_input_port("audio")
            .expect("the port was settled");
        assert_eq!(declaration.sample_rate, 48_000);
        assert_eq!(declaration.channels, Some(2));
        assert_eq!(declaration.dtype, "f32");
        assert_eq!(declaration.window_size, 512);
        assert_eq!(declaration.hop, 512);
    }

    /// A processor stopped and started again opens its device afresh, and the
    /// format it gets that time is the answer — never the previous run's.
    #[test]
    fn resolving_a_second_time_replaces_the_contract_rather_than_keeping_the_first() {
        let contracts = DeviceMatchedAudioWindowContractsByInputPort::default();
        for sample_rate in [44_100, 48_000] {
            contracts.settle_for_input_port(
                "audio",
                ResolvedAudioWindowContract::from_a_device_stream_format(
                    &AudioWindowContractMatchingADeviceStream {
                        device_stream_format: a_device_stream_format(sample_rate, 1),
                        window_size_in_per_channel_samples: 256,
                        hop_in_per_channel_samples: 256,
                    },
                )
                .expect("a device format resolves"),
            );
        }

        assert_eq!(
            contracts
                .settled_for_input_port("audio")
                .expect("the port was settled")
                .sample_rate,
            48_000
        );
    }
}

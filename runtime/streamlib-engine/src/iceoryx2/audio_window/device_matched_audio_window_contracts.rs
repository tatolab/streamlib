// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only


use std::collections::HashMap;

use parking_lot::Mutex;

use super::ResolvedAudioWindowContract;
use crate::core::context::AudioStreamFormat;
use crate::core::descriptors::AudioWindowContractDeclaredValues;

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

#[derive(Debug, Default)]
pub struct DeviceMatchedAudioWindowContractsByInputPort {
    settled: Mutex<HashMap<String, ResolvedAudioWindowContract>>,
}

impl DeviceMatchedAudioWindowContractsByInputPort {
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

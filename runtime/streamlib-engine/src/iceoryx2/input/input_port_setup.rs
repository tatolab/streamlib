// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use crate::core::error::{Error, Result};
use crate::iceoryx2::audio_window::{
    AudioWindowAccumulator, AudioWindowContractMatchingADeviceStream,
    DeviceMatchedAudioWindowContractsByInputPort, LatestQueuedSourceAudioFormat,
    ResolvedAudioWindowContract, queued_audio_window_frame_measure,
};
use crate::iceoryx2::input_port_borrow_quota::DEFAULT_INPUT_PORT_BORROW_QUOTA;
use crate::iceoryx2::mailbox::{PortMailbox, PortMailboxEvictionNotice};
use crate::iceoryx2::read_mode::ReadMode;

use super::{
    InputMailboxesInner, InstalledInputPortAudioWindowing, PortConfig, SharedAudioWindowStage,
    WindowedPortInboundLink, WindowedPortStageInstallation,
};

/// One windowed port's mailbox, stage and read mode, built from the contract
/// that drives them.
///
/// Written once because two callers must produce exactly the same port: the
/// wiring path, for a contract already settled when the link is wired, and the
/// settle, for one that arrives at `setup()`.
fn windowed_port_config(
    port: &str,
    read_mode: ReadMode,
    contract: ResolvedAudioWindowContract,
) -> PortConfig {
    // Shared by the mailbox's measure, which writes every arriving bag's
    // format into it, and the stage, which reads it so its readiness floor is
    // exact before it has consumed anything.
    let latest_queued_source_audio_format = Arc::new(LatestQueuedSourceAudioFormat::default());
    PortConfig {
        mailbox: PortMailbox::new(contract.windowed_port_mailbox_depth(), read_mode)
            .measuring_every_queued_frame_with(queued_audio_window_frame_measure(
                contract,
                Arc::clone(&latest_queued_source_audio_format),
            )),
        audio_windowing: InstalledInputPortAudioWindowing::Windowed(Arc::new(
            parking_lot::Mutex::new(AudioWindowAccumulator::new(
                port,
                contract,
                latest_queued_source_audio_format,
            )),
        )),
        dropped_bag_counter_of_the_bag_last_handed_to_the_reader: None,
        borrow_quota: DEFAULT_INPUT_PORT_BORROW_QUOTA,
    }
}

/// The one-shot notice a `match_device` port installs on the mailbox it holds
/// while its contract is unsettled.
///
/// Every other port that evicts is a consumer falling behind, and the per-link
/// count says so on its own. This one is not: an unsettled port has no contract
/// to window by, so it hands its reader nothing however much arrives, and the
/// climbing count reads as a consumer that is merely slow.
///
/// The state is reached only by a port being drained while unsettled, and the
/// dominant way that happens is a link wired onto the port after its processor's
/// `setup()` has been and gone without settling it — the branch the spawn-time
/// refusal cannot cover, because a port with no link at `setup()` time is not in
/// the map to be refused. Such a port stays unsettled for the rest of the run.
/// Said once per port rather than once per bag: nothing about the state changes
/// between one evicted bag and the next.
fn notice_that_a_bag_was_lost_at_a_port_whose_match_device_contract_is_unsettled(
    port: &str,
    depth: usize,
) -> PortMailboxEvictionNotice {
    let port_name = port.to_string();
    let already_said = std::sync::atomic::AtomicBool::new(false);
    Arc::new(move || {
        if already_said.swap(true, std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        tracing::warn!(
            port = %port_name,
            mailbox_depth = depth,
            "input port evicted a bag while its `audio_window = match_device` contract is \
             unsettled: with no contract to window by it hands its reader nothing, so this \
             loss is not a consumer falling behind. If this port's processor already ran \
             `setup()` without settling it — a link wired onto the port after the fact — \
             it stays this way for the rest of the run"
        );
    })
}

impl InputMailboxesInner {
    /// Add a mailbox for the given port with the specified buffer
    /// size and read mode.
    pub fn add_port(&self, port: &str, buffer_size: usize, read_mode: ReadMode) {
        tracing::debug!(
            port = port,
            buffer_size = buffer_size,
            read_mode = ?read_mode,
            "InputMailboxes: add_port"
        );
        self.ports.lock().insert(
            port.to_string(),
            PortConfig {
                mailbox: PortMailbox::new(buffer_size, read_mode),
                audio_windowing: InstalledInputPortAudioWindowing::NotWindowed,
                dropped_bag_counter_of_the_bag_last_handed_to_the_reader: None,
                borrow_quota: DEFAULT_INPUT_PORT_BORROW_QUOTA,
            },
        );
    }

    /// Add a mailbox for a port whose window contract is `match_device` and is
    /// not settled yet.
    ///
    /// This is what a `match_device` port looks like while nothing has settled
    /// it: countable, and readable by nobody. Two things reach the state — the
    /// compiler wiring every link before it releases any processor into
    /// `setup()`, and a link wired onto the port after that `setup()` has been
    /// and gone without settling it. Only the second is ever drained while it
    /// lasts, and it lasts for the rest of the run. A bag arriving here is
    /// queued and, on overrun, evicted and counted against its own inbound
    /// link, exactly as at any other port — and, unlike at any other port, said
    /// out loud once, because an unsettled port hands its reader nothing and
    /// the count alone reads as a consumer that is merely slow.
    ///
    /// `depth` comes from the caller the way [`Self::add_port`]'s does, rather
    /// than being assumed from the profile a window contract forces: there is
    /// no contract to size it from yet, and the settle re-sizes it from one and
    /// carries anything queued across.
    pub fn add_port_awaiting_its_device_stream_format(
        &self,
        port: &str,
        depth: usize,
        read_mode: ReadMode,
    ) {
        tracing::debug!(
            port = port,
            buffer_size = depth,
            read_mode = ?read_mode,
            "InputMailboxes: add_port_awaiting_its_device_stream_format"
        );
        self.ports.lock().insert(
            port.to_string(),
            PortConfig {
                mailbox: PortMailbox::new(depth, read_mode).reporting_every_eviction_to(
                    notice_that_a_bag_was_lost_at_a_port_whose_match_device_contract_is_unsettled(
                        port, depth,
                    ),
                ),
                audio_windowing: InstalledInputPortAudioWindowing::AwaitingItsDeviceStreamFormat,
                dropped_bag_counter_of_the_bag_last_handed_to_the_reader: None,
                borrow_quota: DEFAULT_INPUT_PORT_BORROW_QUOTA,
            },
        );
    }

    /// Add a mailbox for a port that declared a window contract, with the
    /// stage that honours it.
    ///
    /// The depth is the contract's rather than the delivery profile's: a
    /// window is a span of queued blocks, and `ORDERED_DEPTH` cannot hold a
    /// one-second rolling window's worth. Still engine-chosen, still not
    /// authorable — the contract is a declaration, not a depth dial.
    pub fn add_windowed_port(
        &self,
        port: &str,
        read_mode: ReadMode,
        contract: ResolvedAudioWindowContract,
    ) {
        tracing::debug!(
            port = port,
            buffer_size = contract.windowed_port_mailbox_depth(),
            read_mode = ?read_mode,
            sample_rate = contract.sample_rate,
            channels = %contract.rendered_channel_count(),
            window_size = contract.window_size,
            hop = contract.hop,
            "InputMailboxes: add_windowed_port"
        );
        self.ports.lock().insert(
            port.to_string(),
            windowed_port_config(port, read_mode, contract),
        );
    }

    /// Settle a `match_device` port's contract from the format of the device
    /// stream its own processor just opened, installing the stage that honours
    /// it.
    ///
    /// Called from `setup()`, where the capability typestate is Full — the same
    /// phase in which a processor requests a window.
    ///
    /// **Only a port that declared the sentinel and is still waiting on it can
    /// be settled.** A wired port carries what its author declared, so the port
    /// itself is the check: one that declared its values, or none at all, or
    /// one already settled, is refused naming what it is. Without that a
    /// processor could window a port whose author asked for nothing, or replace
    /// a declared contract with its own — and `graph` would go on rendering the
    /// author's declaration while the stage ran something else.
    ///
    /// The settled values outlive the port itself, so a link wired later —
    /// after `setup()` already ran — finds them rather than a sentinel, and
    /// `graph` renders what the device gave rather than what the declaration
    /// asked for. A port not yet wired has no declaration here to check
    /// against; the wiring path is what reads the settled values, and it reads
    /// them only for a port whose declaration is the sentinel.
    pub(crate) fn settle_a_ports_device_matched_audio_window_contract(
        &self,
        port: &str,
        matching: &AudioWindowContractMatchingADeviceStream,
    ) -> Result<()> {
        let contract = ResolvedAudioWindowContract::from_a_device_stream_format(matching).map_err(
            |refusal| {
                Error::Configuration(format!(
                    "input port '{port}' resolving `audio_window = match_device` from its \
                     device stream produced {refusal}"
                ))
            },
        )?;

        let mut ports = self.ports.lock();
        if let Some(existing) = ports.get_mut(port) {
            if let Some(refusal) = existing.audio_windowing.why_it_cannot_be_settled() {
                return Err(Error::Configuration(format!(
                    "input port '{port}' cannot be settled from a device stream: {refusal}. \
                     Only a port declaring `audio_window = match_device` resolves from the \
                     device its processor opened"
                )));
            }
            let mut settled = windowed_port_config(port, existing.mailbox.read_mode(), contract);
            settled.borrow_quota = existing.borrow_quota;
            // Anything that arrived while the contract was unsettled moves into
            // the mailbox the contract sized, rather than being dropped where
            // no counter would see it.
            existing
                .mailbox
                .hand_every_queued_frame_over_to(&settled.mailbox);
            *existing = settled;
        }
        self.device_matched_audio_window_contracts
            .settle_for_input_port(port, contract);
        drop(ports);

        tracing::info!(
            port = port,
            sample_rate = contract.sample_rate,
            channels = %contract.rendered_channel_count(),
            window_size = contract.window_size,
            hop = contract.hop,
            "InputMailboxes: `audio_window = match_device` settled from the device stream"
        );
        Ok(())
    }

    /// This processor's settled `match_device` contracts, shared with the graph
    /// node so `graph` renders the resolved values off the port that resolved
    /// them.
    pub(crate) fn device_matched_audio_window_contracts(
        &self,
    ) -> Arc<DeviceMatchedAudioWindowContractsByInputPort> {
        Arc::clone(&self.device_matched_audio_window_contracts)
    }

    /// The input ports still holding an unsettled `match_device` sentinel,
    /// named.
    ///
    /// Asked once, after `setup()` returns: by then the only processor that
    /// could have settled one has had its chance, so a port still here belongs
    /// to a processor that opens no device stream and is a wiring error.
    pub(crate) fn input_ports_still_awaiting_their_device_stream_format(&self) -> Vec<String> {
        self.ports
            .lock()
            .iter()
            .filter(|(_, config)| {
                matches!(
                    config.audio_windowing,
                    InstalledInputPortAudioWindowing::AwaitingItsDeviceStreamFormat
                )
            })
            .map(|(port, _)| port.clone())
            .collect()
    }

    /// Whether `stage` is still the stage installed on `port`, and the one link
    /// feeding the port if it is.
    ///
    /// Taken under both locks at once, so the link is the one wired with that
    /// stage: a window is cut from bags rather than being one, so no entry
    /// carries the link, but a windowed port takes exactly one (a second is
    /// refused at wire time).
    pub(super) fn installation_of_a_windowed_ports_stage(
        &self,
        port: &str,
        stage: &SharedAudioWindowStage,
    ) -> WindowedPortStageInstallation {
        let subscribers_and_listener = self.inbound_link_subscribers_and_listener.lock();
        if !self
            .ports
            .lock()
            .get(port)
            .is_some_and(|port_config| port_config.audio_windowing.installs_stage(stage))
        {
            return WindowedPortStageInstallation::Replaced;
        }
        WindowedPortStageInstallation::StillInstalled {
            feeding_link: subscribers_and_listener
                .the_only_subscriber_bound_to_local_port(port)
                .map(|only| WindowedPortInboundLink {
                    link_id: only.link_id.clone(),
                    inbound_link_name: only.inbound_link_name.clone(),
                    dropped_bag_counter: only.dropped_bag_counter.clone(),
                    discarded_sample_counter: only.discarded_sample_counter.clone(),
                }),
        }
    }
}

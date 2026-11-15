// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The service loop: answering connects, admitting and refusing connections, and routing each peer's requests until the service stops.

use super::surface_share_request_dispatch_and_gpu_pool_handlers::answer_a_connected_peers_request;
use super::*;

impl MachSurfaceShareServiceLoop {
    pub(super) fn run(mut self) {
        loop {
            let until_the_next_admission_deadline = self
                .connections_waiting_for_admission
                .iter()
                .map(|waiting| waiting.admission_deadline)
                .min()
                .map(|deadline| deadline.saturating_duration_since(Instant::now()));
            match receive_surface_share_mach_traffic(
                self.port_set.as_raw_name(),
                &mut self.receive_buffer,
                until_the_next_admission_deadline,
            ) {
                Ok(ReceivedSurfaceShareMachTraffic::Message(message)) => {
                    if message.received_on_port == self.control_receive_right.as_raw_name() {
                        if message.sender.pid != self.this_process_pid {
                            tracing::warn!(
                                "[Surface share] ignored a control message from pid {}",
                                message.sender.pid
                            );
                        } else if message.message_id == SERVICE_STOP_MESSAGE_ID {
                            break;
                        } else {
                            self.settle_connections_waiting_for_admission();
                        }
                    } else if message.received_on_port == self.connect_receive_right.as_raw_name() {
                        self.answer_connect(message);
                    } else {
                        self.answer_peer_request(message);
                    }
                }
                Ok(ReceivedSurfaceShareMachTraffic::DeadName { dead_name }) => {
                    self.forget_every_client_answering_on(dead_name.as_raw_name());
                }
                Ok(_) => {}
                Err(timed_out) if timed_out.kind() == io::ErrorKind::TimedOut => {}
                Err(malformed) if malformed.kind() == io::ErrorKind::InvalidData => {
                    tracing::warn!("[Surface share] {}", malformed);
                }
                Err(receive_failure) => {
                    tracing::error!(
                        "[Surface share] the Mach surface service can no longer receive, and \
                         stops: {}",
                        receive_failure
                    );
                    break;
                }
            }
            self.refuse_connections_past_their_admission_deadline();
        }
    }

    fn answer_connect(&mut self, message: ReceivedSurfaceShareMachMessage) {
        let ReceivedSurfaceShareMachMessage {
            message_id,
            reply_send_right,
            sender,
            ..
        } = message;
        if message_id != SURFACE_SHARE_MACH_CONNECT_MESSAGE_ID {
            tracing::warn!(
                "[Surface share] ignored message id {:#x} from pid {} on the connect port",
                message_id,
                sender.pid
            );
            return;
        }
        let Some(reply_send_right) = reply_send_right else {
            tracing::warn!(
                "[Surface share] ignored a connect from pid {} with no reply port",
                sender.pid
            );
            return;
        };
        match self
            .helper_process_admissions
            .verdict_for(sender, self.this_process_pid)
        {
            SurfaceShareConnectionAdmissionVerdict::Admitted => {
                self.accept_connection(reply_send_right, sender);
            }
            SurfaceShareConnectionAdmissionVerdict::NotYetAdmitted
                if self.connections_waiting_for_admission.len()
                    < MAX_CONNECTIONS_WAITING_FOR_ADMISSION =>
            {
                tracing::debug!(
                    "[Surface share] pid {} connected before its admission; waiting up to {:?}",
                    sender.pid,
                    self.unadmitted_connection_wait_budget
                );
                self.connections_waiting_for_admission
                    .push(ConnectionWaitingForAdmission {
                        reply_send_right,
                        sender,
                        admission_deadline: Instant::now() + self.unadmitted_connection_wait_budget,
                    });
            }
            SurfaceShareConnectionAdmissionVerdict::NotYetAdmitted => refuse_connection(
                reply_send_right,
                sender,
                "too many connections are already waiting for admission",
            ),
            SurfaceShareConnectionAdmissionVerdict::Refused(reason) => {
                refuse_connection(reply_send_right, sender, &reason);
            }
        }
    }

    fn accept_connection(
        &mut self,
        reply_send_right: OwnedMachSendRight,
        sender: SurfaceShareMachSenderAuditIdentity,
    ) {
        match self.open_connection_request_port(&reply_send_right) {
            Ok(request_receive_right) => {
                tracing::debug!(
                    "[Surface share] opened a connection for pid {} (pid version {})",
                    sender.pid,
                    sender.pidversion
                );
                self.connected_peers.insert(
                    request_receive_right.as_raw_name(),
                    ConnectedSurfaceSharePeer {
                        request_receive_right,
                        reply_send_right,
                        sender,
                        lease_holder: self.state.check_out_leases().mint_holder_id(),
                        observed_runtime_id: None,
                        pool_slot_keys_handed_an_iosurface:
                            PoolSlotKeysHandedAnIOSurfaceOnThisConnection::default(),
                        claimant_process_handed_a_column: None,
                    },
                );
            }
            Err(unopened) => tracing::warn!(
                "[Surface share] could not open a connection for pid {}: {}",
                sender.pid,
                unopened
            ),
        }
    }

    fn open_connection_request_port(
        &self,
        reply_send_right: &OwnedMachSendRight,
    ) -> io::Result<OwnedMachReceiveRight> {
        let request_receive_right = OwnedMachReceiveRight::allocate()?;
        self.port_set.insert_member(&request_receive_right)?;
        request_dead_name_notification(
            reply_send_right,
            &self.peer_death_notification_receive_right,
        )?;
        send_surface_share_mach_message(
            reply_send_right,
            None,
            SURFACE_SHARE_MACH_REPLY_MESSAGE_ID,
            br#"{"success":true}"#,
            vec![request_receive_right.make_send_right()?],
            Some(CONNECT_ANSWER_SEND_TIMEOUT),
        )?;
        Ok(request_receive_right)
    }

    fn settle_connections_waiting_for_admission(&mut self) {
        let waiting = std::mem::take(&mut self.connections_waiting_for_admission);
        for connection in waiting {
            match self
                .helper_process_admissions
                .verdict_for(connection.sender, self.this_process_pid)
            {
                SurfaceShareConnectionAdmissionVerdict::Admitted => {
                    self.accept_connection(connection.reply_send_right, connection.sender);
                }
                SurfaceShareConnectionAdmissionVerdict::NotYetAdmitted => {
                    self.connections_waiting_for_admission.push(connection);
                }
                SurfaceShareConnectionAdmissionVerdict::Refused(reason) => {
                    refuse_connection(connection.reply_send_right, connection.sender, &reason);
                }
            }
        }
    }

    fn refuse_connections_past_their_admission_deadline(&mut self) {
        let now = Instant::now();
        let (expired, still_waiting): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.connections_waiting_for_admission)
                .into_iter()
                .partition(|waiting| waiting.admission_deadline <= now);
        self.connections_waiting_for_admission = still_waiting;
        for connection in expired {
            refuse_connection(
                connection.reply_send_right,
                connection.sender,
                &format!(
                    "pid {} is neither this process nor a helper process it admitted",
                    connection.sender.pid
                ),
            );
        }
    }

    fn answer_peer_request(&mut self, message: ReceivedSurfaceShareMachMessage) {
        let Some(peer) = self.connected_peers.get_mut(&message.received_on_port) else {
            tracing::warn!(
                "[Surface share] a request from pid {} arrived on a port no connection owns",
                message.sender.pid
            );
            return;
        };
        if message.sender != peer.sender {
            tracing::warn!(
                "[Surface share] refused a request on pid {} (version {})'s connection from pid \
                 {} (version {})",
                peer.sender.pid,
                peer.sender.pidversion,
                message.sender.pid,
                message.sender.pidversion
            );
            return;
        }
        if message.message_id != SURFACE_SHARE_MACH_REQUEST_MESSAGE_ID {
            tracing::warn!(
                "[Surface share] ignored message id {:#x} on pid {}'s connection",
                message.message_id,
                peer.sender.pid
            );
            return;
        }
        let (response, reply_ports) =
            match serde_json::from_slice::<serde_json::Value>(&message.json_payload) {
                Ok(request) => {
                    latch_the_first_named_runtime_id(&mut peer.observed_runtime_id, &request);
                    answer_a_connected_peers_request(&self.state, &request, message.ports, peer)
                }
                Err(invalid_json) => (
                    serde_json::json!({"error": format!("invalid JSON: {invalid_json}")}),
                    Vec::new(),
                ),
            };
        let answered = send_surface_share_mach_message(
            &peer.reply_send_right,
            None,
            SURFACE_SHARE_MACH_REPLY_MESSAGE_ID,
            response.to_string().as_bytes(),
            reply_ports,
            Some(REPLY_SEND_TIMEOUT),
        );
        if let Err(unsent) = answered {
            tracing::warn!(
                "[Surface share] could not answer pid {}; closing its connection: {}",
                message.sender.pid,
                unsent
            );
            self.close_connection(message.received_on_port, true);
        }
    }

    /// Close every connection that answers on `dead_reply_port`. Send rights
    /// to one port share one name in this task, so a client that opened
    /// several connections on one reply port dies for all of them at once.
    fn forget_every_client_answering_on(&mut self, dead_reply_port: mach_port_name_t) {
        self.connections_waiting_for_admission
            .retain(|waiting| waiting.reply_send_right.as_raw_name() != dead_reply_port);
        let dead_connections: Vec<mach_port_name_t> = self
            .connected_peers
            .iter()
            .filter(|(_, peer)| peer.reply_send_right.as_raw_name() == dead_reply_port)
            .map(|(request_port, _)| *request_port)
            .collect();
        for request_port in dead_connections {
            self.close_connection(request_port, false);
        }
    }

    /// Close one connection, releasing what it held. Its peer's claim table
    /// columns clear once the peer's process has exited, and a peer whose
    /// connection the service dropped is stopped first.
    fn close_connection(
        &mut self,
        request_port: mach_port_name_t,
        the_service_dropped_the_connection: bool,
    ) {
        let Some(mut peer) = self.connected_peers.remove(&request_port) else {
            return;
        };
        if let Some(claimant_process) = peer.claimant_process_handed_a_column.take() {
            let claim_tables = Arc::clone(self.state.check_out_leases());
            let watched = thread::Builder::new()
                .name("surface-share-claimant-exit".into())
                .spawn(move || {
                    claimant_process.clear_its_columns_once_it_has_exited(
                        claim_tables.gpu_pool_claim_tables(),
                        the_service_dropped_the_connection,
                    )
                });
            if let Err(unspawned) = watched {
                tracing::warn!(
                    "[Surface share] could not watch pid {} for its exit, so its claim table \
                     columns stay held: {unspawned}",
                    peer.sender.pid
                );
            }
        }
        let out_of_process_runtime_id = peer
            .observed_runtime_id
            .as_deref()
            .filter(|_| peer.sender.pid != self.this_process_pid);
        release_what_a_closed_connection_held(
            self.state.check_out_leases(),
            peer.lease_holder,
            &format_args!("pid {}'s connection", peer.sender.pid),
            out_of_process_runtime_id.map(|runtime_id| (&self.state as _, runtime_id)),
        );
        drop(peer.request_receive_right);
    }
}

fn refuse_connection(
    reply_send_right: OwnedMachSendRight,
    sender: SurfaceShareMachSenderAuditIdentity,
    reason: &str,
) {
    tracing::warn!(
        "[Surface share] refused a connection from pid {} (version {}): {}",
        sender.pid,
        sender.pidversion,
        reason
    );
    let refusal = serde_json::json!({ "error": reason }).to_string();
    let _ = send_surface_share_mach_message(
        &reply_send_right,
        None,
        SURFACE_SHARE_MACH_REPLY_MESSAGE_ID,
        refusal.as_bytes(),
        Vec::new(),
        Some(CONNECT_ANSWER_SEND_TIMEOUT),
    );
}

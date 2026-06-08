// Copyright (c) 2026 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! MAVLink2 encoder processor — accepts `MavlinkMessage` on `messages_in`
//! and emits MAVLink2-framed `NetworkPacket` payloads on `bytes_out`. The
//! sequence counter is auto-incremented per (system_id, component_id)
//! pair; the input message's `sequence` field is ignored. Outbound
//! `peer_addr` passes through from the input message.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use mavlink::MavHeader;
use mavlink::dialects::common::{
    ATTITUDE_DATA, AttitudeTargetTypemask, COMMAND_LONG_DATA, ENCAPSULATED_DATA_DATA,
    HEARTBEAT_DATA, HIGHRES_IMU_DATA, HighresImuUpdatedFlags, MavAutopilot, MavCmd, MavFrame,
    MavMessage, MavModeFlag, MavState, MavType, PositionTargetTypemask, SET_ATTITUDE_TARGET_DATA,
    SET_POSITION_TARGET_LOCAL_NED_DATA, TIMESYNC_DATA,
};
use num_traits::FromPrimitive;
use streamlib_plugin_sdk::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib_plugin_sdk::sdk::error::{Error, Result};
use streamlib_plugin_sdk::sdk::processors::ReactiveProcessor;

use crate::_generated_::{MavlinkMessage, NetworkPacket};

#[streamlib_plugin_sdk::sdk::processor("MavlinkEncoder")]
pub struct MavlinkEncoderProcessor {
    /// Per-(system_id, component_id) sequence counter. Initialized lazily
    /// on first message for each pair; wraps at 256.
    sequence_counters: HashMap<(u8, u8), u8>,
    messages_encoded: AtomicU64,
    encode_errors: AtomicU64,
}

impl ReactiveProcessor for MavlinkEncoderProcessor::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!(
            default_system_id = self.config.default_system_id,
            default_component_id = self.config.default_component_id,
            "MavlinkEncoder: setup",
        );
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if !self.inputs.has_data("messages_in") {
            return Ok(());
        }
        let msg: MavlinkMessage = self.inputs.read("messages_in")?;

        let default_sys = self.config.default_system_id;
        let default_comp = self.config.default_component_id;

        let (system_id, component_id, peer_addr) = identity(&msg, default_sys, default_comp);
        // Auto-increment per (system_id, component_id) — the input
        // message's `sequence` field is deliberately ignored. The
        // schema field exists for visibility on decode, not as an
        // encoder input; threading it through user code would let a
        // bug there corrupt a real MAVLink sequence-tracking link.
        let sequence = self
            .sequence_counters
            .entry((system_id, component_id))
            .and_modify(|s| *s = s.wrapping_add(1))
            .or_insert(0);
        let header = MavHeader {
            system_id,
            component_id,
            sequence: *sequence,
        };

        let mav_msg = match convert_to_mavlink(&msg) {
            Ok(m) => m,
            Err(e) => {
                let n = self.encode_errors.fetch_add(1, Ordering::Relaxed) + 1;
                if n == 1 || n.is_power_of_two() {
                    tracing::warn!(
                        error = %e,
                        encode_errors_total = n,
                        "MavlinkEncoder: failed to lift typed message",
                    );
                }
                return Ok(());
            }
        };

        let mut payload = Vec::with_capacity(MAVLINK_V2_MAX_FRAME_BYTES);
        if let Err(e) = mavlink::write_v2_msg(&mut payload, header, &mav_msg) {
            let n = self.encode_errors.fetch_add(1, Ordering::Relaxed) + 1;
            if n == 1 || n.is_power_of_two() {
                tracing::warn!(
                    error = %e,
                    encode_errors_total = n,
                    "MavlinkEncoder: write_v2_msg failed",
                );
            }
            return Ok(());
        }

        let packet = NetworkPacket {
            payload,
            peer_addr,
            // Outbound packets don't carry a recv timestamp; the upstream
            // UdpSink ignores this field on send anyway.
            timestamp_ns: "0".to_string(),
        };

        self.outputs.write("bytes_out", &packet)?;

        let n = self.messages_encoded.fetch_add(1, Ordering::Relaxed) + 1;
        if n == 1 {
            tracing::info!(
                system_id,
                component_id,
                "MavlinkEncoder: first message encoded"
            );
        }
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!(
            messages_encoded = self.messages_encoded.load(Ordering::Relaxed),
            encode_errors = self.encode_errors.load(Ordering::Relaxed),
            "MavlinkEncoder: teardown",
        );
        Ok(())
    }
}

/// MAVLink 2 max wire frame: 1 STX + 10 header + 255 payload + 2 CRC +
/// 13 signature = 281 bytes. Round up for the Vec capacity hint.
const MAVLINK_V2_MAX_FRAME_BYTES: usize = 288;

fn identity(msg: &MavlinkMessage, default_sys: u8, default_comp: u8) -> (u8, u8, String) {
    let (sys, comp, peer) = match msg {
        MavlinkMessage::Heartbeat(d) => (d.system_id, d.component_id, d.peer_addr.clone()),
        MavlinkMessage::Attitude(d) => (d.system_id, d.component_id, d.peer_addr.clone()),
        MavlinkMessage::HighresImu(d) => (d.system_id, d.component_id, d.peer_addr.clone()),
        MavlinkMessage::SetPositionTargetLocalNed(d) => {
            (d.system_id, d.component_id, d.peer_addr.clone())
        }
        MavlinkMessage::SetAttitudeTarget(d) => (d.system_id, d.component_id, d.peer_addr.clone()),
        MavlinkMessage::Timesync(d) => (d.system_id, d.component_id, d.peer_addr.clone()),
        MavlinkMessage::CommandLong(d) => (d.system_id, d.component_id, d.peer_addr.clone()),
        MavlinkMessage::EncapsulatedData(d) => (d.system_id, d.component_id, d.peer_addr.clone()),
        MavlinkMessage::LocalPositionNed(d) => (d.system_id, d.component_id, d.peer_addr.clone()),
        MavlinkMessage::Odometry(d) => (d.system_id, d.component_id, d.peer_addr.clone()),
        MavlinkMessage::ActuatorOutputStatus(d) => {
            (d.system_id, d.component_id, d.peer_addr.clone())
        }
        MavlinkMessage::Collision(d) => (d.system_id, d.component_id, d.peer_addr.clone()),
        MavlinkMessage::CommandAck(d) => (d.system_id, d.component_id, d.peer_addr.clone()),
    };
    let sys = if sys == 0 { default_sys } else { sys };
    let comp = if comp == 0 { default_comp } else { comp };
    (sys, comp, peer)
}

fn convert_to_mavlink(msg: &MavlinkMessage) -> Result<MavMessage> {
    Ok(match msg {
        MavlinkMessage::Heartbeat(d) => MavMessage::HEARTBEAT(HEARTBEAT_DATA {
            custom_mode: d.custom_mode,
            mavtype: lift_enum::<MavType>(d.mavtype, "HEARTBEAT.mavtype")?,
            autopilot: lift_enum::<MavAutopilot>(d.autopilot, "HEARTBEAT.autopilot")?,
            base_mode: MavModeFlag::from_bits_truncate(d.base_mode),
            system_status: lift_enum::<MavState>(d.system_status, "HEARTBEAT.system_status")?,
            mavlink_version: d.mavlink_version,
        }),
        MavlinkMessage::Attitude(d) => MavMessage::ATTITUDE(ATTITUDE_DATA {
            time_boot_ms: d.time_boot_ms,
            roll: d.roll,
            pitch: d.pitch,
            yaw: d.yaw,
            rollspeed: d.rollspeed,
            pitchspeed: d.pitchspeed,
            yawspeed: d.yawspeed,
        }),
        MavlinkMessage::HighresImu(d) => {
            let time_usec: u64 = d.time_usec.parse().map_err(|e| {
                Error::Configuration(format!(
                    "MavlinkEncoder: HIGHRES_IMU.time_usec is not a valid u64 ({:?}): {e}",
                    d.time_usec
                ))
            })?;
            MavMessage::HIGHRES_IMU(HIGHRES_IMU_DATA {
                time_usec,
                xacc: d.xacc,
                yacc: d.yacc,
                zacc: d.zacc,
                xgyro: d.xgyro,
                ygyro: d.ygyro,
                zgyro: d.zgyro,
                xmag: d.xmag,
                ymag: d.ymag,
                zmag: d.zmag,
                abs_pressure: d.abs_pressure,
                diff_pressure: d.diff_pressure,
                pressure_alt: d.pressure_alt,
                temperature: d.temperature,
                fields_updated: HighresImuUpdatedFlags::from_bits_truncate(d.fields_updated),
                id: d.id,
            })
        }
        MavlinkMessage::SetPositionTargetLocalNed(d) => {
            MavMessage::SET_POSITION_TARGET_LOCAL_NED(SET_POSITION_TARGET_LOCAL_NED_DATA {
                time_boot_ms: d.time_boot_ms,
                x: d.x,
                y: d.y,
                z: d.z,
                vx: d.vx,
                vy: d.vy,
                vz: d.vz,
                afx: d.afx,
                afy: d.afy,
                afz: d.afz,
                yaw: d.yaw,
                yaw_rate: d.yaw_rate,
                type_mask: PositionTargetTypemask::from_bits_truncate(d.type_mask),
                target_system: d.target_system,
                target_component: d.target_component,
                coordinate_frame: lift_enum::<MavFrame>(
                    d.coordinate_frame,
                    "SET_POSITION_TARGET_LOCAL_NED.coordinate_frame",
                )?,
            })
        }
        MavlinkMessage::SetAttitudeTarget(d) => {
            if d.q.len() != 4 {
                return Err(Error::Configuration(format!(
                    "MavlinkEncoder: SET_ATTITUDE_TARGET.q must have exactly 4 elements, got {}",
                    d.q.len()
                )));
            }
            if d.thrust_body.len() != 3 {
                return Err(Error::Configuration(format!(
                    "MavlinkEncoder: SET_ATTITUDE_TARGET.thrust_body must have exactly 3 elements, got {}",
                    d.thrust_body.len()
                )));
            }
            let q = [d.q[0], d.q[1], d.q[2], d.q[3]];
            let thrust_body = [d.thrust_body[0], d.thrust_body[1], d.thrust_body[2]];
            MavMessage::SET_ATTITUDE_TARGET(SET_ATTITUDE_TARGET_DATA {
                time_boot_ms: d.time_boot_ms,
                q,
                body_roll_rate: d.body_roll_rate,
                body_pitch_rate: d.body_pitch_rate,
                body_yaw_rate: d.body_yaw_rate,
                thrust: d.thrust,
                target_system: d.target_system,
                target_component: d.target_component,
                type_mask: AttitudeTargetTypemask::from_bits_truncate(d.type_mask),
                thrust_body,
            })
        }
        MavlinkMessage::Timesync(d) => {
            let tc1: i64 = d.tc1.parse().map_err(|e| {
                Error::Configuration(format!(
                    "MavlinkEncoder: TIMESYNC.tc1 is not a valid i64 ({:?}): {e}",
                    d.tc1
                ))
            })?;
            let ts1: i64 = d.ts1.parse().map_err(|e| {
                Error::Configuration(format!(
                    "MavlinkEncoder: TIMESYNC.ts1 is not a valid i64 ({:?}): {e}",
                    d.ts1
                ))
            })?;
            MavMessage::TIMESYNC(TIMESYNC_DATA {
                tc1,
                ts1,
                target_system: d.target_system,
                target_component: d.target_component,
            })
        }
        MavlinkMessage::CommandLong(d) => MavMessage::COMMAND_LONG(COMMAND_LONG_DATA {
            target_system: d.target_system,
            target_component: d.target_component,
            command: MavCmd::from_u16(d.command).ok_or_else(|| {
                Error::Configuration(format!(
                    "MavlinkEncoder: COMMAND_LONG.command = {} is not a known MAV_CMD in the common dialect",
                    d.command
                ))
            })?,
            confirmation: d.confirmation,
            param1: d.param1,
            param2: d.param2,
            param3: d.param3,
            param4: d.param4,
            param5: d.param5,
            param6: d.param6,
            param7: d.param7,
        }),
        MavlinkMessage::EncapsulatedData(d) => {
            // ENCAPSULATED_DATA carries a fixed 253-byte payload on the wire;
            // pad or truncate the variable-length schema bytes to fit.
            let mut data = [0u8; 253];
            let n = d.data.len().min(253);
            data[..n].copy_from_slice(&d.data[..n]);
            MavMessage::ENCAPSULATED_DATA(ENCAPSULATED_DATA_DATA { seqnr: d.seqnr, data })
        }
        // SIM->PILOT telemetry the decoder surfaces for consumers but the
        // pilot never transmits. They have no encode path on purpose: the
        // racer receives these, it does not emit them.
        MavlinkMessage::LocalPositionNed(_)
        | MavlinkMessage::Odometry(_)
        | MavlinkMessage::ActuatorOutputStatus(_)
        | MavlinkMessage::Collision(_)
        | MavlinkMessage::CommandAck(_) => {
            return Err(Error::Configuration(
                "MavlinkEncoder: LOCAL_POSITION_NED / ODOMETRY / ACTUATOR_OUTPUT_STATUS / \
                 COLLISION / COMMAND_ACK are SIM->PILOT telemetry surfaced by the decoder but \
                 never transmitted by the pilot — decode-only, not encodable"
                    .to_string(),
            ));
        }
    })
}

/// Lift a raw `u8` from the wire schema into a typed rust-mavlink enum
/// variant. Returns `Error::Configuration` when the value doesn't match
/// any declared discriminant — the schema documents these fields as
/// enum-bearing uint8, so silent substitution to GENERIC would
/// misrepresent caller intent on the wire.
fn lift_enum<E: FromPrimitive>(value: u8, ctx: &'static str) -> Result<E> {
    E::from_u8(value).ok_or_else(|| {
        Error::Configuration(format!(
            "MavlinkEncoder: {ctx} = {value} is not a known enum discriminant"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::_generated_::tatolab__mavlink::mavlink_message::{
        MavlinkMessageAttitude, MavlinkMessageCommandLong, MavlinkMessageEncapsulatedData,
        MavlinkMessageHeartbeat, MavlinkMessageHighresImu, MavlinkMessageLocalPositionNed,
        MavlinkMessageSetAttitudeTarget, MavlinkMessageSetPositionTargetLocalNed,
        MavlinkMessageTimesync,
    };

    fn make_heartbeat(sys: u8, comp: u8) -> MavlinkMessage {
        MavlinkMessage::Heartbeat(MavlinkMessageHeartbeat {
            system_id: sys,
            component_id: comp,
            sequence: 0,
            peer_addr: String::new(),
            timestamp_ns: "0".to_string(),
            custom_mode: 0,
            mavtype: 2,    // MAV_TYPE_QUADROTOR
            autopilot: 12, // MAV_AUTOPILOT_PX4
            base_mode: 0,
            system_status: 4, // MAV_STATE_ACTIVE
            mavlink_version: 3,
        })
    }

    fn make_attitude() -> MavlinkMessage {
        MavlinkMessage::Attitude(MavlinkMessageAttitude {
            system_id: 1,
            component_id: 1,
            sequence: 0,
            peer_addr: String::new(),
            timestamp_ns: "0".to_string(),
            time_boot_ms: 12345,
            roll: 0.1,
            pitch: -0.2,
            yaw: 1.5,
            rollspeed: 0.01,
            pitchspeed: -0.02,
            yawspeed: 0.15,
        })
    }

    fn make_highres_imu() -> MavlinkMessage {
        MavlinkMessage::HighresImu(MavlinkMessageHighresImu {
            system_id: 1,
            component_id: 1,
            sequence: 0,
            peer_addr: String::new(),
            timestamp_ns: "0".to_string(),
            time_usec: "1234567890123456".to_string(),
            xacc: 0.1,
            yacc: 0.2,
            zacc: -9.81,
            xgyro: 0.01,
            ygyro: 0.02,
            zgyro: -0.03,
            xmag: 0.5,
            ymag: -0.4,
            zmag: 0.9,
            abs_pressure: 1013.25,
            diff_pressure: 0.5,
            pressure_alt: 100.0,
            temperature: 22.5,
            fields_updated: 0b0000_0000_0000_0111,
            id: 0,
        })
    }

    fn make_set_position_target() -> MavlinkMessage {
        MavlinkMessage::SetPositionTargetLocalNed(MavlinkMessageSetPositionTargetLocalNed {
            system_id: 255,
            component_id: 190,
            sequence: 0,
            peer_addr: String::new(),
            timestamp_ns: "0".to_string(),
            time_boot_ms: 12345,
            target_system: 1,
            target_component: 1,
            coordinate_frame: 1, // MAV_FRAME_LOCAL_NED
            type_mask: 0b0000_1111_1111_1000,
            x: 1.0,
            y: 2.0,
            z: -3.0,
            vx: 0.1,
            vy: 0.2,
            vz: -0.3,
            afx: 0.0,
            afy: 0.0,
            afz: 0.0,
            yaw: 0.5,
            yaw_rate: 0.05,
        })
    }

    fn make_set_attitude_target() -> MavlinkMessage {
        MavlinkMessage::SetAttitudeTarget(MavlinkMessageSetAttitudeTarget {
            system_id: 255,
            component_id: 190,
            sequence: 0,
            peer_addr: String::new(),
            timestamp_ns: "0".to_string(),
            time_boot_ms: 12345,
            target_system: 1,
            target_component: 1,
            type_mask: 0b0000_0111,
            q: vec![1.0, 0.0, 0.0, 0.0],
            body_roll_rate: 0.0,
            body_pitch_rate: 0.0,
            body_yaw_rate: 0.0,
            thrust: 0.5,
            thrust_body: vec![0.0, 0.0, 0.0],
        })
    }

    fn make_timesync() -> MavlinkMessage {
        MavlinkMessage::Timesync(MavlinkMessageTimesync {
            system_id: 1,
            component_id: 1,
            sequence: 0,
            peer_addr: String::new(),
            timestamp_ns: "0".to_string(),
            tc1: "1234567890".to_string(),
            ts1: "9876543210".to_string(),
            target_system: 0,
            target_component: 0,
        })
    }

    fn make_command_long() -> MavlinkMessage {
        MavlinkMessage::CommandLong(MavlinkMessageCommandLong {
            system_id: 255,
            component_id: 190,
            sequence: 0,
            peer_addr: String::new(),
            timestamp_ns: "0".to_string(),
            target_system: 1,
            target_component: 1,
            command: 400, // MAV_CMD_COMPONENT_ARM_DISARM
            confirmation: 0,
            param1: 1.0, // 1 = arm
            param2: 0.0,
            param3: 0.0,
            param4: 0.0,
            param5: 0.0,
            param6: 0.0,
            param7: 0.0,
        })
    }

    fn make_encapsulated_data() -> MavlinkMessage {
        // ENCAPSULATED_DATA carries a fixed 253-byte wire payload; build the
        // full width so the round-trip compares equal (the encoder pads to 253).
        let mut data = vec![0u8; 253];
        data[0] = 1; // AGP race-status discriminator (data_type)
        data[1] = 0xAB;
        data[252] = 0xCD;
        MavlinkMessage::EncapsulatedData(MavlinkMessageEncapsulatedData {
            system_id: 1,
            component_id: 1,
            sequence: 0,
            peer_addr: String::new(),
            timestamp_ns: "0".to_string(),
            seqnr: 7,
            data,
        })
    }

    /// Encode → write_v2_msg → read_v2_msg → decode → compare. Exercises
    /// the encoder's typed lift, the wire framing, the decoder's typed
    /// drop, and the schema's discriminator path end-to-end. Mentally
    /// reverting any one of these (e.g. dropping the FromPrimitive enum
    /// reconstruction, breaking the bitflags lift, mis-ordering the
    /// header fields) breaks the round-trip.
    fn assert_round_trip(input: MavlinkMessage) {
        let mav_msg = convert_to_mavlink(&input).expect("convert_to_mavlink");
        let header = mavlink::MavHeader {
            system_id: identity(&input, 0, 0).0,
            component_id: identity(&input, 0, 0).1,
            sequence: 42,
        };
        let mut wire = Vec::new();
        mavlink::write_v2_msg(&mut wire, header, &mav_msg).expect("write_v2_msg");

        let peer = "127.0.0.1:14550";
        let timestamp = "1000000";
        let decoded = crate::mavlink_decoder::decode_one(&wire, peer, timestamp)
            .expect("decode_one ok")
            .expect("decoded into a supported variant");

        // peer_addr / timestamp_ns are populated by the decoder from the
        // network metadata, so they won't match the input's empty values
        // — null them out on both sides before compare.
        let normalize = |mut msg: MavlinkMessage| -> MavlinkMessage {
            match &mut msg {
                MavlinkMessage::Heartbeat(d) => {
                    d.peer_addr.clear();
                    d.timestamp_ns = "0".to_string();
                    d.sequence = 0;
                }
                MavlinkMessage::Attitude(d) => {
                    d.peer_addr.clear();
                    d.timestamp_ns = "0".to_string();
                    d.sequence = 0;
                }
                MavlinkMessage::HighresImu(d) => {
                    d.peer_addr.clear();
                    d.timestamp_ns = "0".to_string();
                    d.sequence = 0;
                }
                MavlinkMessage::SetPositionTargetLocalNed(d) => {
                    d.peer_addr.clear();
                    d.timestamp_ns = "0".to_string();
                    d.sequence = 0;
                }
                MavlinkMessage::SetAttitudeTarget(d) => {
                    d.peer_addr.clear();
                    d.timestamp_ns = "0".to_string();
                    d.sequence = 0;
                }
                MavlinkMessage::Timesync(d) => {
                    d.peer_addr.clear();
                    d.timestamp_ns = "0".to_string();
                    d.sequence = 0;
                }
                MavlinkMessage::CommandLong(d) => {
                    d.peer_addr.clear();
                    d.timestamp_ns = "0".to_string();
                    d.sequence = 0;
                }
                MavlinkMessage::EncapsulatedData(d) => {
                    d.peer_addr.clear();
                    d.timestamp_ns = "0".to_string();
                    d.sequence = 0;
                }
                // Decode-only telemetry variants: convert_to_mavlink rejects
                // them, so they never reach assert_round_trip — but normalize
                // them anyway to keep the match exhaustive (so a future
                // encodable variant can't silently skip normalization).
                MavlinkMessage::LocalPositionNed(d) => {
                    d.peer_addr.clear();
                    d.timestamp_ns = "0".to_string();
                    d.sequence = 0;
                }
                MavlinkMessage::Odometry(d) => {
                    d.peer_addr.clear();
                    d.timestamp_ns = "0".to_string();
                    d.sequence = 0;
                }
                MavlinkMessage::ActuatorOutputStatus(d) => {
                    d.peer_addr.clear();
                    d.timestamp_ns = "0".to_string();
                    d.sequence = 0;
                }
                MavlinkMessage::Collision(d) => {
                    d.peer_addr.clear();
                    d.timestamp_ns = "0".to_string();
                    d.sequence = 0;
                }
                MavlinkMessage::CommandAck(d) => {
                    d.peer_addr.clear();
                    d.timestamp_ns = "0".to_string();
                    d.sequence = 0;
                }
            }
            msg
        };

        assert_eq!(normalize(input), normalize(decoded));
    }

    #[test]
    fn round_trip_heartbeat() {
        assert_round_trip(make_heartbeat(1, 1));
    }

    #[test]
    fn round_trip_attitude() {
        assert_round_trip(make_attitude());
    }

    #[test]
    fn round_trip_highres_imu() {
        assert_round_trip(make_highres_imu());
    }

    #[test]
    fn round_trip_set_position_target_local_ned() {
        assert_round_trip(make_set_position_target());
    }

    #[test]
    fn round_trip_set_attitude_target() {
        assert_round_trip(make_set_attitude_target());
    }

    #[test]
    fn round_trip_timesync() {
        assert_round_trip(make_timesync());
    }

    #[test]
    fn round_trip_command_long() {
        assert_round_trip(make_command_long());
    }

    #[test]
    fn round_trip_encapsulated_data() {
        assert_round_trip(make_encapsulated_data());
    }

    #[test]
    fn command_long_sim_reset_roundtrips() {
        // The AGP sim-reset (cmd 31000) is within rust-mavlink's MavCmd, so it
        // encodes and survives the wire roundtrip — the attempt-cycle reset works
        // through the same COMMAND_LONG path as arm (400).
        let mut msg = make_command_long();
        if let MavlinkMessage::CommandLong(d) = &mut msg {
            d.command = 31000;
            d.param1 = 0.0;
        }
        assert_round_trip(msg);
    }

    #[test]
    fn invalid_quaternion_length_rejects() {
        let msg = MavlinkMessage::SetAttitudeTarget(MavlinkMessageSetAttitudeTarget {
            system_id: 1,
            component_id: 1,
            sequence: 0,
            peer_addr: String::new(),
            timestamp_ns: "0".to_string(),
            time_boot_ms: 0,
            target_system: 1,
            target_component: 1,
            type_mask: 0,
            q: vec![1.0, 0.0, 0.0], // only 3 elements — invalid
            body_roll_rate: 0.0,
            body_pitch_rate: 0.0,
            body_yaw_rate: 0.0,
            thrust: 0.0,
            thrust_body: vec![0.0, 0.0, 0.0],
        });
        match convert_to_mavlink(&msg) {
            Err(Error::Configuration(s)) => {
                assert!(s.contains("q must have exactly 4 elements"), "got: {s}");
            }
            other => panic!("expected configuration error on q.len() != 4, got {other:?}"),
        }
    }

    #[test]
    fn invalid_thrust_body_length_rejects() {
        let msg = MavlinkMessage::SetAttitudeTarget(MavlinkMessageSetAttitudeTarget {
            system_id: 1,
            component_id: 1,
            sequence: 0,
            peer_addr: String::new(),
            timestamp_ns: "0".to_string(),
            time_boot_ms: 0,
            target_system: 1,
            target_component: 1,
            type_mask: 0,
            q: vec![1.0, 0.0, 0.0, 0.0],
            body_roll_rate: 0.0,
            body_pitch_rate: 0.0,
            body_yaw_rate: 0.0,
            thrust: 0.0,
            thrust_body: vec![0.0, 0.0], // only 2 elements — invalid
        });
        match convert_to_mavlink(&msg) {
            Err(Error::Configuration(s)) => {
                assert!(
                    s.contains("thrust_body must have exactly 3 elements"),
                    "got: {s}"
                );
            }
            other => {
                panic!("expected configuration error on thrust_body.len() != 3, got {other:?}")
            }
        }
    }

    #[test]
    fn invalid_int64_string_rejects() {
        let msg = MavlinkMessage::Timesync(MavlinkMessageTimesync {
            system_id: 1,
            component_id: 1,
            sequence: 0,
            peer_addr: String::new(),
            timestamp_ns: "0".to_string(),
            tc1: "not-a-number".to_string(),
            ts1: "0".to_string(),
            target_system: 0,
            target_component: 0,
        });
        match convert_to_mavlink(&msg) {
            Err(Error::Configuration(s)) => {
                assert!(s.contains("tc1 is not a valid i64"), "got: {s}");
            }
            other => panic!("expected configuration error on bad tc1 string, got {other:?}"),
        }
    }

    #[test]
    fn invalid_enum_discriminant_rejects() {
        let msg = MavlinkMessage::Heartbeat(MavlinkMessageHeartbeat {
            system_id: 1,
            component_id: 1,
            sequence: 0,
            peer_addr: String::new(),
            timestamp_ns: "0".to_string(),
            custom_mode: 0,
            mavtype: 250, // not a known MAV_TYPE discriminant
            autopilot: 12,
            base_mode: 0,
            system_status: 4,
            mavlink_version: 3,
        });
        match convert_to_mavlink(&msg) {
            Err(Error::Configuration(s)) => {
                assert!(s.contains("mavtype"), "expected mavtype in error: {s}");
                assert!(s.contains("250"), "expected the bad value in error: {s}");
            }
            other => panic!("expected configuration error on out-of-range mavtype, got {other:?}"),
        }
    }

    #[test]
    fn telemetry_messages_are_decode_only() {
        // LOCAL_POSITION_NED (like ODOMETRY / ACTUATOR_OUTPUT_STATUS / COLLISION
        // / COMMAND_ACK) is SIM->PILOT telemetry the decoder surfaces but the
        // pilot never sends — encode must refuse rather than fabricate a frame.
        let msg = MavlinkMessage::LocalPositionNed(MavlinkMessageLocalPositionNed {
            system_id: 1,
            component_id: 1,
            sequence: 0,
            peer_addr: String::new(),
            timestamp_ns: "0".to_string(),
            time_boot_ms: 0,
            x: 0.0,
            y: 0.0,
            z: 0.0,
            vx: 0.0,
            vy: 0.0,
            vz: 0.0,
        });
        match convert_to_mavlink(&msg) {
            Err(Error::Configuration(s)) => {
                assert!(
                    s.contains("decode-only"),
                    "expected decode-only rejection, got: {s}"
                );
            }
            other => panic!("expected decode-only rejection, got {other:?}"),
        }
    }
}

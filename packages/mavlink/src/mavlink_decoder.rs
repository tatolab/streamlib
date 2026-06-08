// Copyright (c) 2026 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! MAVLink2 decoder processor — accepts `NetworkPacket` payloads on
//! `bytes_in` and emits typed `MavlinkMessage` variants on `messages_out`.
//! Parse failures (malformed frames, unknown msgid, CRC fail) are
//! counted and logged on first occurrence + powers-of-two thereafter;
//! they do NOT abort the processor.

use std::io::Cursor;
use std::sync::atomic::{AtomicU64, Ordering};

use mavlink::dialects::common::MavMessage;
use mavlink::peek_reader::PeekReader;
use streamlib_plugin_sdk::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib_plugin_sdk::sdk::error::Result;
use streamlib_plugin_sdk::sdk::processors::ReactiveProcessor;

use crate::_generated_::tatolab__mavlink::mavlink_message::{
    MavlinkMessageActuatorOutputStatus, MavlinkMessageAttitude, MavlinkMessageCollision,
    MavlinkMessageCommandAck, MavlinkMessageCommandLong, MavlinkMessageEncapsulatedData,
    MavlinkMessageHeartbeat, MavlinkMessageHighresImu, MavlinkMessageLocalPositionNed,
    MavlinkMessageOdometry, MavlinkMessageSetAttitudeTarget,
    MavlinkMessageSetPositionTargetLocalNed, MavlinkMessageTimesync,
};
use crate::_generated_::{MavlinkMessage, NetworkPacket};

#[streamlib_plugin_sdk::sdk::processor("MavlinkDecoder")]
pub struct MavlinkDecoderProcessor {
    messages_decoded: AtomicU64,
    parse_errors: AtomicU64,
}

impl ReactiveProcessor for MavlinkDecoderProcessor::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!(
            warn_on_parse_error = ?self.config.warn_on_parse_error,
            "MavlinkDecoder: setup",
        );
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if !self.inputs.has_data("bytes_in") {
            return Ok(());
        }
        let packet: NetworkPacket = self.inputs.read("bytes_in")?;

        let warn = self.config.warn_on_parse_error.unwrap_or(true);

        match decode_one(&packet.payload, &packet.peer_addr, &packet.timestamp_ns) {
            Ok(Some(msg)) => {
                self.outputs.write("messages_out", &msg)?;
                let n = self.messages_decoded.fetch_add(1, Ordering::Relaxed) + 1;
                if n == 1 {
                    tracing::info!("MavlinkDecoder: first message decoded");
                }
                Ok(())
            }
            Ok(None) => Ok(()),
            Err(err) => {
                let n = self.parse_errors.fetch_add(1, Ordering::Relaxed) + 1;
                if warn && (n == 1 || n.is_power_of_two()) {
                    tracing::warn!(
                        error = %err,
                        peer_addr = %packet.peer_addr,
                        bytes = packet.payload.len(),
                        parse_errors_total = n,
                        "MavlinkDecoder: dropping malformed frame",
                    );
                }
                Ok(())
            }
        }
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!(
            messages_decoded = self.messages_decoded.load(Ordering::Relaxed),
            parse_errors = self.parse_errors.load(Ordering::Relaxed),
            "MavlinkDecoder: teardown",
        );
        Ok(())
    }
}

/// Parse one MAVLink2 frame out of a byte slice and convert into the
/// streamlib `MavlinkMessage` tagged union. Returns `Ok(None)` when the
/// message is a valid MAVLink2 frame of a common-dialect type this decoder
/// does not surface; returns `Err` for wire-level corruption.
pub(crate) fn decode_one(
    payload: &[u8],
    peer_addr: &str,
    timestamp_ns: &str,
) -> std::result::Result<Option<MavlinkMessage>, mavlink::error::MessageReadError> {
    let mut reader = PeekReader::new(Cursor::new(payload));
    let (header, msg) = mavlink::read_v2_msg::<MavMessage, _>(&mut reader)?;
    Ok(convert(msg, header, peer_addr, timestamp_ns))
}

fn convert(
    msg: MavMessage,
    header: mavlink::MavHeader,
    peer_addr: &str,
    timestamp_ns: &str,
) -> Option<MavlinkMessage> {
    use mavlink::dialects::common::MavMessage::*;
    let system_id = header.system_id;
    let component_id = header.component_id;
    let sequence = header.sequence;
    let peer_addr = peer_addr.to_string();
    let timestamp_ns = timestamp_ns.to_string();

    Some(match msg {
        HEARTBEAT(d) => MavlinkMessage::Heartbeat(MavlinkMessageHeartbeat {
            system_id,
            component_id,
            sequence,
            peer_addr,
            timestamp_ns,
            custom_mode: d.custom_mode,
            mavtype: d.mavtype as u8,
            autopilot: d.autopilot as u8,
            base_mode: d.base_mode.bits(),
            system_status: d.system_status as u8,
            mavlink_version: d.mavlink_version,
        }),
        ATTITUDE(d) => MavlinkMessage::Attitude(MavlinkMessageAttitude {
            system_id,
            component_id,
            sequence,
            peer_addr,
            timestamp_ns,
            time_boot_ms: d.time_boot_ms,
            roll: d.roll,
            pitch: d.pitch,
            yaw: d.yaw,
            rollspeed: d.rollspeed,
            pitchspeed: d.pitchspeed,
            yawspeed: d.yawspeed,
        }),
        HIGHRES_IMU(d) => MavlinkMessage::HighresImu(MavlinkMessageHighresImu {
            system_id,
            component_id,
            sequence,
            peer_addr,
            timestamp_ns,
            time_usec: d.time_usec.to_string(),
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
            fields_updated: d.fields_updated.bits(),
            id: d.id,
        }),
        SET_POSITION_TARGET_LOCAL_NED(d) => {
            MavlinkMessage::SetPositionTargetLocalNed(MavlinkMessageSetPositionTargetLocalNed {
                system_id,
                component_id,
                sequence,
                peer_addr,
                timestamp_ns,
                time_boot_ms: d.time_boot_ms,
                target_system: d.target_system,
                target_component: d.target_component,
                coordinate_frame: d.coordinate_frame as u8,
                type_mask: d.type_mask.bits(),
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
            })
        }
        SET_ATTITUDE_TARGET(d) => {
            MavlinkMessage::SetAttitudeTarget(MavlinkMessageSetAttitudeTarget {
                system_id,
                component_id,
                sequence,
                peer_addr,
                timestamp_ns,
                time_boot_ms: d.time_boot_ms,
                target_system: d.target_system,
                target_component: d.target_component,
                type_mask: d.type_mask.bits(),
                q: d.q.to_vec(),
                body_roll_rate: d.body_roll_rate,
                body_pitch_rate: d.body_pitch_rate,
                body_yaw_rate: d.body_yaw_rate,
                thrust: d.thrust,
                thrust_body: d.thrust_body.to_vec(),
            })
        }
        TIMESYNC(d) => MavlinkMessage::Timesync(MavlinkMessageTimesync {
            system_id,
            component_id,
            sequence,
            peer_addr,
            timestamp_ns,
            tc1: d.tc1.to_string(),
            ts1: d.ts1.to_string(),
            target_system: d.target_system,
            target_component: d.target_component,
        }),
        COMMAND_LONG(d) => MavlinkMessage::CommandLong(MavlinkMessageCommandLong {
            system_id,
            component_id,
            sequence,
            peer_addr,
            timestamp_ns,
            target_system: d.target_system,
            target_component: d.target_component,
            command: d.command as u16,
            confirmation: d.confirmation,
            param1: d.param1,
            param2: d.param2,
            param3: d.param3,
            param4: d.param4,
            param5: d.param5,
            param6: d.param6,
            param7: d.param7,
        }),
        ENCAPSULATED_DATA(d) => MavlinkMessage::EncapsulatedData(MavlinkMessageEncapsulatedData {
            system_id,
            component_id,
            sequence,
            peer_addr,
            timestamp_ns,
            seqnr: d.seqnr,
            data: d.data.to_vec(),
        }),
        LOCAL_POSITION_NED(d) => MavlinkMessage::LocalPositionNed(MavlinkMessageLocalPositionNed {
            system_id,
            component_id,
            sequence,
            peer_addr,
            timestamp_ns,
            time_boot_ms: d.time_boot_ms,
            x: d.x,
            y: d.y,
            z: d.z,
            vx: d.vx,
            vy: d.vy,
            vz: d.vz,
        }),
        ODOMETRY(d) => MavlinkMessage::Odometry(MavlinkMessageOdometry {
            system_id,
            component_id,
            sequence,
            peer_addr,
            timestamp_ns,
            time_usec: d.time_usec.to_string(),
            x: d.x,
            y: d.y,
            z: d.z,
            q: d.q.to_vec(),
            vx: d.vx,
            vy: d.vy,
            vz: d.vz,
            rollspeed: d.rollspeed,
            pitchspeed: d.pitchspeed,
            yawspeed: d.yawspeed,
            pose_covariance: d.pose_covariance.to_vec(),
            velocity_covariance: d.velocity_covariance.to_vec(),
            frame_id: d.frame_id as u8,
            child_frame_id: d.child_frame_id as u8,
            reset_counter: d.reset_counter,
            estimator_type: d.estimator_type as u8,
            quality: d.quality,
        }),
        ACTUATOR_OUTPUT_STATUS(d) => {
            MavlinkMessage::ActuatorOutputStatus(MavlinkMessageActuatorOutputStatus {
                system_id,
                component_id,
                sequence,
                peer_addr,
                timestamp_ns,
                time_usec: d.time_usec.to_string(),
                active: d.active,
                actuator: d.actuator.to_vec(),
            })
        }
        COLLISION(d) => MavlinkMessage::Collision(MavlinkMessageCollision {
            system_id,
            component_id,
            sequence,
            peer_addr,
            timestamp_ns,
            id: d.id,
            time_to_minimum_delta: d.time_to_minimum_delta,
            altitude_minimum_delta: d.altitude_minimum_delta,
            horizontal_minimum_delta: d.horizontal_minimum_delta,
            src: d.src as u8,
            action: d.action as u8,
            threat_level: d.threat_level as u8,
        }),
        COMMAND_ACK(d) => MavlinkMessage::CommandAck(MavlinkMessageCommandAck {
            system_id,
            component_id,
            sequence,
            peer_addr,
            timestamp_ns,
            command: d.command as u16,
            result: d.result as u8,
            progress: d.progress,
            result_param2: d.result_param2,
            target_system: d.target_system,
            target_component: d.target_component,
        }),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_bytes_return_error() {
        let garbage = vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE];
        let result = decode_one(&garbage, "127.0.0.1:14550", "0");
        assert!(
            result.is_err(),
            "garbage bytes must surface a parser error, got: {result:?}"
        );
    }

    #[test]
    fn empty_bytes_return_error() {
        let result = decode_one(&[], "127.0.0.1:14550", "0");
        assert!(
            result.is_err(),
            "empty payload cannot contain a MAVLink frame"
        );
    }

    #[test]
    fn truncated_frame_returns_error() {
        let truncated = vec![0xFD, 0x00];
        let result = decode_one(&truncated, "127.0.0.1:14550", "0");
        assert!(
            result.is_err(),
            "truncated frame must error, got: {result:?}"
        );
    }

    /// Spec-anchor test — the byte sequence below was generated by
    /// pymavlink (the MAVSDK-compatible Python implementation) on
    /// 2026-05-17 from this Python program:
    ///
    /// ```python
    /// from pymavlink.dialects.v20 import common as mavlink2
    /// mav = mavlink2.MAVLink(file=None, srcSystem=1, srcComponent=1)
    /// mav.seq = 42
    /// msg = mavlink2.MAVLink_heartbeat_message(
    ///     type=mavlink2.MAV_TYPE_QUADROTOR,        # 2
    ///     autopilot=mavlink2.MAV_AUTOPILOT_PX4,     # 12
    ///     base_mode=0, custom_mode=0,
    ///     system_status=mavlink2.MAV_STATE_ACTIVE,  # 4
    ///     mavlink_version=3,
    /// )
    /// print(msg.pack(mav, force_mavlink1=False).hex())
    /// ```
    ///
    /// Decoding it via our decoder must produce the same typed
    /// HEARTBEAT we'd build by hand. Mentally reverting any wire-
    /// format detail in rust-mavlink (CRC algorithm, header layout,
    /// payload zero-trim) would break this test — it locks the
    /// upstream-spec contract independent of any rust-mavlink self-
    /// roundtrip.
    #[test]
    fn decodes_pymavlink_heartbeat_byte_sequence() {
        let heartbeat_bytes = [
            0xFD, 0x09, 0x00, 0x00, 0x2A, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x02, 0x0C, 0x00, 0x04, 0x03, 0x42, 0xB2,
        ];
        let decoded = decode_one(&heartbeat_bytes, "10.0.0.5:14550", "1700000000")
            .expect("pymavlink HEARTBEAT bytes must decode")
            .expect("HEARTBEAT is a supported variant");

        match decoded {
            MavlinkMessage::Heartbeat(d) => {
                assert_eq!(d.system_id, 1);
                assert_eq!(d.component_id, 1);
                assert_eq!(d.sequence, 42);
                assert_eq!(d.peer_addr, "10.0.0.5:14550");
                assert_eq!(d.timestamp_ns, "1700000000");
                assert_eq!(d.custom_mode, 0);
                assert_eq!(d.mavtype, 2, "MAV_TYPE_QUADROTOR");
                assert_eq!(d.autopilot, 12, "MAV_AUTOPILOT_PX4");
                assert_eq!(d.base_mode, 0);
                assert_eq!(d.system_status, 4, "MAV_STATE_ACTIVE");
                assert_eq!(d.mavlink_version, 3);
            }
            other => panic!("expected HEARTBEAT, got {other:?}"),
        }
    }

    use mavlink::MavHeader;
    use mavlink::dialects::common::{
        ACTUATOR_OUTPUT_STATUS_DATA, COLLISION_DATA, COMMAND_ACK_DATA, LOCAL_POSITION_NED_DATA,
        MavCmd, MavCollisionAction, MavCollisionSrc, MavCollisionThreatLevel, MavEstimatorType,
        MavFrame, MavResult, ODOMETRY_DATA,
    };

    /// Build a MAVLink2 frame from a typed `MavMessage`, then run it back
    /// through `decode_one` — the same wire path the sim drives.
    fn build_and_decode(seq: u8, msg: MavMessage) -> MavlinkMessage {
        let header = MavHeader {
            system_id: 1,
            component_id: 1,
            sequence: seq,
        };
        let mut wire = Vec::new();
        mavlink::write_v2_msg(&mut wire, header, &msg).expect("write_v2_msg");
        decode_one(&wire, "10.0.0.5:14550", "1700000000")
            .expect("frame must decode")
            .expect("a surfaced variant")
    }

    #[test]
    fn decodes_local_position_ned_velocity() {
        let decoded = build_and_decode(
            10,
            MavMessage::LOCAL_POSITION_NED(LOCAL_POSITION_NED_DATA {
                time_boot_ms: 12345,
                x: 1.0,
                y: 2.0,
                z: -3.0,
                vx: -20.6,
                vy: 4.2,
                vz: 4.7,
            }),
        );
        match decoded {
            MavlinkMessage::LocalPositionNed(d) => {
                assert_eq!(d.system_id, 1);
                assert_eq!(d.peer_addr, "10.0.0.5:14550");
                assert_eq!(d.time_boot_ms, 12345);
                assert_eq!(d.x.to_bits(), 1.0f32.to_bits());
                assert_eq!(
                    d.vx.to_bits(),
                    (-20.6f32).to_bits(),
                    "world-frame X velocity"
                );
                assert_eq!(d.vy.to_bits(), 4.2f32.to_bits());
                assert_eq!(d.vz.to_bits(), 4.7f32.to_bits());
            }
            other => panic!("expected LocalPositionNed, got {other:?}"),
        }
    }

    #[test]
    // MAV_FRAME_BODY_NED (8) is deprecated upstream but is exactly what the
    // AGP sim sends as ODOMETRY.child_frame_id — assert against reality.
    #[allow(deprecated)]
    fn decodes_odometry_velocity_orientation_rates_and_reset() {
        let decoded = build_and_decode(
            11,
            MavMessage::ODOMETRY(ODOMETRY_DATA {
                time_usec: 1_234_567_890_123_456,
                x: 1.0,
                y: 2.0,
                z: -3.0,
                q: [0.0005, -0.155, 0.00004, -0.988],
                vx: 7.565,
                vy: -15.539,
                vz: -12.839,
                rollspeed: 0.1,
                pitchspeed: -0.2,
                yawspeed: 0.3,
                pose_covariance: [0.0; 21],
                velocity_covariance: [0.0; 21],
                frame_id: MavFrame::MAV_FRAME_LOCAL_NED,
                child_frame_id: MavFrame::MAV_FRAME_BODY_NED,
                reset_counter: 6,
                estimator_type: MavEstimatorType::MAV_ESTIMATOR_TYPE_VISION,
                quality: 100,
            }),
        );
        match decoded {
            MavlinkMessage::Odometry(d) => {
                assert_eq!(d.time_usec, "1234567890123456", "uint64 rides as a string");
                assert_eq!(d.q.len(), 4);
                assert_eq!(d.q[0].to_bits(), 0.0005f32.to_bits());
                assert_eq!(d.vx.to_bits(), 7.565f32.to_bits(), "body-frame X velocity");
                assert_eq!(d.vy.to_bits(), (-15.539f32).to_bits());
                assert_eq!(d.vz.to_bits(), (-12.839f32).to_bits());
                assert_eq!(d.rollspeed.to_bits(), 0.1f32.to_bits());
                assert_eq!(d.frame_id, MavFrame::MAV_FRAME_LOCAL_NED as u8);
                assert_eq!(d.child_frame_id, MavFrame::MAV_FRAME_BODY_NED as u8);
                assert_eq!(d.reset_counter, 6, "reset counter for teleport rejection");
                assert_eq!(
                    d.estimator_type,
                    MavEstimatorType::MAV_ESTIMATOR_TYPE_VISION as u8,
                );
                assert_eq!(d.quality, 100);
                assert_eq!(d.pose_covariance.len(), 21);
                assert_eq!(d.velocity_covariance.len(), 21);
            }
            other => panic!("expected Odometry, got {other:?}"),
        }
    }

    #[test]
    fn decodes_actuator_output_status() {
        let mut actuator = [0.0f32; 32];
        actuator[0] = 0.25;
        actuator[3] = 0.75;
        let decoded = build_and_decode(
            12,
            MavMessage::ACTUATOR_OUTPUT_STATUS(ACTUATOR_OUTPUT_STATUS_DATA {
                time_usec: 999,
                active: 0b1111,
                actuator,
            }),
        );
        match decoded {
            MavlinkMessage::ActuatorOutputStatus(d) => {
                assert_eq!(d.time_usec, "999");
                assert_eq!(d.active, 0b1111);
                assert_eq!(d.actuator.len(), 32);
                assert_eq!(d.actuator[0].to_bits(), 0.25f32.to_bits());
                assert_eq!(d.actuator[3].to_bits(), 0.75f32.to_bits());
            }
            other => panic!("expected ActuatorOutputStatus, got {other:?}"),
        }
    }

    #[test]
    fn decodes_collision() {
        let decoded = build_and_decode(
            13,
            MavMessage::COLLISION(COLLISION_DATA {
                id: 1001,
                time_to_minimum_delta: 0.5,
                altitude_minimum_delta: 1.5,
                horizontal_minimum_delta: 2.5,
                src: MavCollisionSrc::MAV_COLLISION_SRC_ADSB,
                action: MavCollisionAction::MAV_COLLISION_ACTION_NONE,
                threat_level: MavCollisionThreatLevel::MAV_COLLISION_THREAT_LEVEL_HIGH,
            }),
        );
        match decoded {
            MavlinkMessage::Collision(d) => {
                assert_eq!(d.id, 1001, "AGP gate collision id");
                assert_eq!(d.horizontal_minimum_delta.to_bits(), 2.5f32.to_bits());
                assert_eq!(
                    d.threat_level,
                    MavCollisionThreatLevel::MAV_COLLISION_THREAT_LEVEL_HIGH as u8,
                );
            }
            other => panic!("expected Collision, got {other:?}"),
        }
    }

    #[test]
    fn decodes_command_ack() {
        let decoded = build_and_decode(
            14,
            MavMessage::COMMAND_ACK(COMMAND_ACK_DATA {
                command: MavCmd::MAV_CMD_COMPONENT_ARM_DISARM,
                result: MavResult::MAV_RESULT_ACCEPTED,
                progress: 50,
                result_param2: 7,
                target_system: 1,
                target_component: 1,
            }),
        );
        match decoded {
            MavlinkMessage::CommandAck(d) => {
                assert_eq!(d.command, 400, "MAV_CMD_COMPONENT_ARM_DISARM");
                assert_eq!(d.result, MavResult::MAV_RESULT_ACCEPTED as u8);
                assert_eq!(d.progress, 50);
                assert_eq!(d.result_param2, 7);
                assert_eq!(d.target_system, 1);
            }
            other => panic!("expected CommandAck, got {other:?}"),
        }
    }
}

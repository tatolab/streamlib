// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Which machine's monotonic clock a stamp crossing the mesh was taken on.
//!
//! Every stamp on the data plane is the machine's monotonic clock, whose epoch
//! is that machine's own boot. Two stamps from different boots are two readings
//! of two unrelated clocks, and subtracting them is meaningless — so a stamp
//! that crosses the mesh carries the identity of the clock that produced it.
//!
//! The identity is the kernel's boot-session UUID: `/proc/sys/kernel/random/boot_id`
//! on Linux and `kern.bootsessionuuid` on Apple. Deliberately the boot alone
//! and nothing else, so a container and its host — which share a kernel and so
//! share a monotonic epoch — read as one clock. That is the opposite of what
//! [`HostIdentity`] asks, which pairs the same boot id with the pid namespace
//! precisely to tell a container apart from its host.
//!
//! [`HostIdentity`]: crate::core::runtime::mesh::HostIdentity

use std::sync::OnceLock;

/// How many bytes one clock identity is, in memory and on the wire.
pub const MACHINE_CLOCK_IDENTITY_BYTES: usize = 16;

/// The machine whose monotonic clock produced a stamp.
///
/// All-zero is the nil UUID and means this platform named no clock, so two
/// unidentified machines are never read as sharing one.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct MachineClockIdentity([u8; MACHINE_CLOCK_IDENTITY_BYTES]);

impl MachineClockIdentity {
    /// The identity a platform reports for no clock at all.
    pub const UNIDENTIFIED: Self = Self([0u8; MACHINE_CLOCK_IDENTITY_BYTES]);

    /// This machine's, read once — a boot id cannot change without a reboot,
    /// which ends this process.
    pub fn of_this_machine() -> Self {
        static OF_THIS_MACHINE: OnceLock<MachineClockIdentity> = OnceLock::new();
        *OF_THIS_MACHINE.get_or_init(|| {
            #[cfg(target_os = "linux")]
            {
                crate::linux::machine_clock_identity::read_this_machines_clock_identity()
            }
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            {
                crate::apple::machine_clock_identity::read_this_machines_clock_identity()
            }
            #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios")))]
            {
                Self::UNIDENTIFIED
            }
        })
    }

    /// The identity a boot-session UUID's text names, or
    /// [`Self::UNIDENTIFIED`] when the text is not one.
    ///
    /// Both platforms answer with UUID text and they disagree on case — Linux
    /// writes it lowercase, Apple uppercase — so the parse is the one place
    /// either answer is turned into bytes.
    pub fn of_the_machine_whose_boot_session_uuid_reads(boot_session_uuid: &str) -> Self {
        let mut identity = [0u8; MACHINE_CLOCK_IDENTITY_BYTES];
        let mut hex_digits = boot_session_uuid
            .as_bytes()
            .iter()
            .copied()
            .filter(|byte| *byte != b'-');
        for byte in identity.iter_mut() {
            let (Some(high), Some(low)) = (hex_digits.next(), hex_digits.next()) else {
                return Self::UNIDENTIFIED;
            };
            let (Some(high), Some(low)) = (hex_digit_value(high), hex_digit_value(low)) else {
                return Self::UNIDENTIFIED;
            };
            *byte = (high << 4) | low;
        }
        if hex_digits.next().is_some() {
            return Self::UNIDENTIFIED;
        }
        Self(identity)
    }

    /// Read an identity off the wire, where it is these sixteen bytes verbatim.
    pub fn from_wire_bytes(wire_bytes: [u8; MACHINE_CLOCK_IDENTITY_BYTES]) -> Self {
        Self(wire_bytes)
    }

    /// This identity on the wire.
    pub fn to_wire_bytes(self) -> [u8; MACHINE_CLOCK_IDENTITY_BYTES] {
        self.0
    }

    /// Whether this platform named no clock at all.
    pub fn is_unidentified(&self) -> bool {
        *self == Self::UNIDENTIFIED
    }
}

/// The value one hex digit carries, or `None` for a byte that is not one.
fn hex_digit_value(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

impl std::fmt::Debug for MachineClockIdentity {
    /// The canonical lowercase UUID text, because the derived rendering of
    /// sixteen bytes is unreadable in the failure message of any test that
    /// compares two of these.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (position, byte) in self.0.iter().enumerate() {
            if matches!(position, 4 | 6 | 8 | 10) {
                write!(formatter, "-")?;
            }
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The canonical UUID text reads as the bytes it spells, in the order it
    /// spells them — the property that makes two machines' identities
    /// comparable at all.
    #[test]
    fn a_boot_session_uuid_reads_as_the_sixteen_bytes_it_spells() {
        assert_eq!(
            MachineClockIdentity::of_the_machine_whose_boot_session_uuid_reads(
                "2f1c8a30-6b4e-4d5a-9a11-2c7f0d5e8b93"
            )
            .to_wire_bytes(),
            [
                0x2f, 0x1c, 0x8a, 0x30, 0x6b, 0x4e, 0x4d, 0x5a, 0x9a, 0x11, 0x2c, 0x7f, 0x0d,
                0x5e, 0x8b, 0x93
            ]
        );
    }

    /// Linux writes the boot id lowercase and Apple writes it uppercase, so
    /// the same machine read through either must not read as two clocks.
    #[test]
    fn the_two_platforms_spellings_of_one_uuid_are_one_identity() {
        assert_eq!(
            MachineClockIdentity::of_the_machine_whose_boot_session_uuid_reads(
                "2f1c8a30-6b4e-4d5a-9a11-2c7f0d5e8b93"
            ),
            MachineClockIdentity::of_the_machine_whose_boot_session_uuid_reads(
                "2F1C8A30-6B4E-4D5A-9A11-2C7F0D5E8B93"
            )
        );
    }

    /// Text that is not a boot-session UUID names no clock, rather than a
    /// half-read one another machine could then be compared against.
    #[test]
    fn text_that_is_not_a_boot_session_uuid_names_no_clock() {
        for not_one in [
            "",
            "-",
            "2f1c8a30-6b4e-4d5a-9a11-2c7f0d5e8b9",
            "2f1c8a30-6b4e-4d5a-9a11-2c7f0d5e8b930",
            "2f1c8a30-6b4e-4d5a-9a11-2c7f0d5e8b9z",
            "not a uuid at all",
        ] {
            assert!(
                MachineClockIdentity::of_the_machine_whose_boot_session_uuid_reads(not_one)
                    .is_unidentified(),
                "{not_one:?} must name no clock"
            );
        }
    }

    /// The nil UUID is what a platform naming no clock renders, so reading it
    /// back must not claim an identity two machines could share.
    #[test]
    fn the_nil_uuid_is_unidentified_and_so_is_the_constant() {
        assert!(
            MachineClockIdentity::of_the_machine_whose_boot_session_uuid_reads(
                "00000000-0000-0000-0000-000000000000"
            )
            .is_unidentified()
        );
        assert!(MachineClockIdentity::UNIDENTIFIED.is_unidentified());
        assert_eq!(
            MachineClockIdentity::UNIDENTIFIED.to_wire_bytes(),
            [0u8; MACHINE_CLOCK_IDENTITY_BYTES]
        );
    }

    /// Every identity survives the wire as itself.
    #[test]
    fn an_identity_round_trips_through_its_wire_bytes() {
        for identity in [
            MachineClockIdentity::of_this_machine(),
            MachineClockIdentity::of_the_machine_whose_boot_session_uuid_reads(
                "2f1c8a30-6b4e-4d5a-9a11-2c7f0d5e8b93",
            ),
            MachineClockIdentity::UNIDENTIFIED,
        ] {
            assert_eq!(
                MachineClockIdentity::from_wire_bytes(identity.to_wire_bytes()),
                identity
            );
        }
    }

    /// This machine names one clock and names the same one every time — a
    /// stamp's clock changing mid-run would read as a peer that re-booted.
    #[test]
    fn this_machine_names_the_same_clock_every_time() {
        assert_eq!(
            MachineClockIdentity::of_this_machine(),
            MachineClockIdentity::of_this_machine()
        );
    }

    /// The readable rendering is the canonical UUID text, which is what a
    /// failing comparison has to show.
    #[test]
    fn the_rendering_is_the_canonical_uuid_text() {
        assert_eq!(
            format!(
                "{:?}",
                MachineClockIdentity::of_the_machine_whose_boot_session_uuid_reads(
                    "2F1C8A30-6B4E-4D5A-9A11-2C7F0D5E8B93"
                )
            ),
            "2f1c8a30-6b4e-4d5a-9a11-2c7f0d5e8b93"
        );
    }
}

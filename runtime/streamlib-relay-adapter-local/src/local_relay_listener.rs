// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The local relay's two listeners. Where each binds is never a setting: the
//! loopback binding is what keeps a private port on the machine.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use streamlib_relay_adapter_sdk::{OutputPortExposureLevel, RelayInstanceId};

const EXPOSURE_LEVELS_SERVED_AT_LOOPBACK: [OutputPortExposureLevel; 2] = [
    OutputPortExposureLevel::Private,
    OutputPortExposureLevel::Public,
];

const EXPOSURE_LEVELS_SERVED_AT_EVERY_INTERFACE: [OutputPortExposureLevel; 1] =
    [OutputPortExposureLevel::Public];

/// One of the local relay's two listeners, each its own `moq-relay` child.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LocalRelayListener {
    /// Bound to loopback alone, serving every exposed port.
    Loopback,
    /// Bound to every interface, serving public ports alone.
    EveryInterface,
}

impl LocalRelayListener {
    /// Both listeners, loopback first.
    pub const BOTH: [Self; 2] = [Self::Loopback, Self::EveryInterface];

    /// The relay instance id this listener is known by.
    pub fn relay_id(self) -> RelayInstanceId {
        RelayInstanceId(
            match self {
                Self::Loopback => "local-loopback",
                Self::EveryInterface => "local-every-interface",
            }
            .to_string(),
        )
    }

    /// The port this listener takes when its settings name none.
    pub fn default_port(self) -> u16 {
        match self {
            Self::Loopback => 4443,
            Self::EveryInterface => 4445,
        }
    }

    /// Where this listener binds on `port`, for QUIC and HTTP alike.
    pub fn bind_address(self, port: u16) -> SocketAddr {
        match self {
            Self::Loopback => SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
            Self::EveryInterface => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port),
        }
    }

    /// Where the runtime on this machine dials this listener on `port`.
    pub fn address_dialed_on_this_machine(self, port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }

    /// The exposure levels whose ports this listener serves.
    pub fn levels_served(self) -> &'static [OutputPortExposureLevel] {
        match self {
            Self::Loopback => &EXPOSURE_LEVELS_SERVED_AT_LOOPBACK,
            Self::EveryInterface => &EXPOSURE_LEVELS_SERVED_AT_EVERY_INTERFACE,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_binds_loopback_alone_and_serves_private_and_public() {
        let bind = LocalRelayListener::Loopback.bind_address(4443);
        assert!(bind.ip().is_loopback());
        assert_eq!(bind.to_string(), "127.0.0.1:4443");
        assert_eq!(
            LocalRelayListener::Loopback.levels_served(),
            &[
                OutputPortExposureLevel::Private,
                OutputPortExposureLevel::Public
            ]
        );
    }

    #[test]
    fn every_interface_binds_the_wildcard_and_serves_public_alone() {
        let bind = LocalRelayListener::EveryInterface.bind_address(4445);
        assert!(bind.ip().is_unspecified());
        assert_eq!(bind.to_string(), "[::]:4445");
        assert_eq!(
            LocalRelayListener::EveryInterface.levels_served(),
            &[OutputPortExposureLevel::Public]
        );
    }

    #[test]
    fn the_two_listeners_take_distinct_ids_and_default_ports() {
        assert_eq!(LocalRelayListener::Loopback.relay_id().0, "local-loopback");
        assert_eq!(
            LocalRelayListener::EveryInterface.relay_id().0,
            "local-every-interface"
        );
        assert_eq!(LocalRelayListener::Loopback.default_port(), 4443);
        assert_eq!(LocalRelayListener::EveryInterface.default_port(), 4445);
    }
}

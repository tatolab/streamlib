// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Where another machine can reach the control plane this runtime hosts.
//!
//! A runtime joins the mesh whether or not it hosts one, and a control plane
//! is hosted after the runtime is constructed — so this is a cell the engine
//! owns and the control plane fills in, read at query time rather than
//! captured at announcement.

use std::net::{IpAddr, Ipv6Addr};

use parking_lot::RwLock;

/// The control plane a runtime hosts, if it hosts one.
///
/// Shared between the runtime that announces it and the control plane
/// processor that binds it, because the two are brought up at different times
/// and neither owns the other.
#[derive(Debug, Default)]
pub struct HostedControlPlaneEndpointRegistry {
    endpoint: RwLock<Option<HostedControlPlaneEndpoint>>,
}

/// What the control plane actually bound.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HostedControlPlaneEndpoint {
    bind_host: String,
    bound_port: u16,
}

impl HostedControlPlaneEndpointRegistry {
    /// Record what the control plane bound, so the mesh can say where to reach
    /// it. The last caller wins: a runtime hosts one control plane.
    pub fn record_what_the_control_plane_bound(&self, bind_host: &str, bound_port: u16) {
        *self.endpoint.write() = Some(HostedControlPlaneEndpoint {
            bind_host: bind_host.to_string(),
            bound_port,
        });
    }

    /// Record that the control plane is gone, so the mesh stops naming an
    /// endpoint nothing answers on.
    pub fn record_that_the_control_plane_is_gone(&self) {
        *self.endpoint.write() = None;
    }

    /// Every URL another machine could reach this control plane at — empty
    /// with no control plane, and empty when the bind covers nothing a remote
    /// caller can route to.
    pub fn urls_another_machine_could_reach_it_at(&self) -> Vec<String> {
        let endpoint = self.endpoint.read().clone();
        let Some(endpoint) = endpoint else {
            return Vec::new();
        };
        control_plane_urls_for(
            &endpoint.bind_host,
            endpoint.bound_port,
            &every_address_of_this_hosts_interfaces(),
        )
    }
}

/// The URLs a bind of `bind_host:bound_port` is reachable at, given the
/// addresses this host's interfaces carry.
///
/// Spelled with the interface addresses passed in so every arm is testable on
/// a machine whose own interfaces say nothing useful.
fn control_plane_urls_for(
    bind_host: &str,
    bound_port: u16,
    every_interface_address: &[IpAddr],
) -> Vec<String> {
    let Ok(bound_address) = bind_host.parse::<IpAddr>() else {
        // A name rather than an address: the engine resolves nothing, and the
        // name is what a caller was told to use.
        return vec![format!("http://{bind_host}:{bound_port}")];
    };

    let covered: Vec<IpAddr> = if bound_address.is_unspecified() {
        every_interface_address
            .iter()
            .copied()
            // A wildcard IPv4 bind covers no IPv6 address; a wildcard IPv6 one
            // covers both, which is what the dual-stack default gives.
            .filter(|address| bound_address.is_ipv6() || address.is_ipv4())
            .collect()
    } else {
        vec![bound_address]
    };

    let mut urls: Vec<String> = covered
        .into_iter()
        .filter(is_reachable_from_another_machine)
        .map(|address| {
            format!(
                "http://{}:{bound_port}",
                bracketed_if_it_is_a_literal(address)
            )
        })
        .collect();
    urls.sort();
    urls.dedup();
    urls
}

/// Whether another machine could route to `address` at all.
fn is_reachable_from_another_machine(address: &IpAddr) -> bool {
    if address.is_loopback() || address.is_unspecified() || address.is_multicast() {
        return false;
    }
    match address {
        IpAddr::V4(address) => !address.is_link_local(),
        // `fe80::/10`. `Ipv6Addr::is_unicast_link_local` is still unstable, so
        // the prefix is read directly.
        IpAddr::V6(address) => !is_link_local_ipv6(address),
    }
}

fn is_link_local_ipv6(address: &Ipv6Addr) -> bool {
    address.segments()[0] & 0xffc0 == 0xfe80
}

/// An IPv6 literal inside a URL is bracketed, per RFC 3986 §3.2.2.
fn bracketed_if_it_is_a_literal(address: IpAddr) -> String {
    match address {
        IpAddr::V4(address) => address.to_string(),
        IpAddr::V6(address) => format!("[{address}]"),
    }
}

/// Every address this host's interfaces carry, loopback and link-local
/// included — the filtering belongs to the caller, which knows what the bind
/// covers.
fn every_address_of_this_hosts_interfaces() -> Vec<IpAddr> {
    let mut first_interface: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: `getifaddrs` writes a list head into the pointer it is given and
    // returns non-zero without writing on failure.
    if unsafe { libc::getifaddrs(&mut first_interface) } != 0 || first_interface.is_null() {
        tracing::debug!(
            "this host reports no network interfaces; the mesh announces no control plane URL"
        );
        return Vec::new();
    }

    let mut addresses = Vec::new();
    let mut interface = first_interface;
    while !interface.is_null() {
        // SAFETY: the walk stops at the null `ifa_next` `getifaddrs` terminates
        // its list with, and the list is alive until `freeifaddrs` below.
        let entry = unsafe { &*interface };
        if let Some(address) = read_one_interface_address(entry.ifa_addr) {
            addresses.push(address);
        }
        interface = entry.ifa_next;
    }

    // SAFETY: `first_interface` is what `getifaddrs` handed back and is freed
    // exactly once, after the walk above has finished reading it.
    unsafe { libc::freeifaddrs(first_interface) };
    addresses
}

/// One `sockaddr`'s address, or `None` for a family that carries none — a
/// packet socket, a null entry for an interface with no address.
fn read_one_interface_address(socket_address: *const libc::sockaddr) -> Option<IpAddr> {
    if socket_address.is_null() {
        return None;
    }
    // SAFETY: the pointer is non-null and points at a `sockaddr` whose
    // `sa_family` says which of the two larger structs it really is; each arm
    // below reads only through the matching type.
    unsafe {
        match (*socket_address).sa_family as libc::c_int {
            libc::AF_INET => {
                let address = &*(socket_address as *const libc::sockaddr_in);
                Some(IpAddr::from(address.sin_addr.s_addr.to_ne_bytes()))
            }
            libc::AF_INET6 => {
                let address = &*(socket_address as *const libc::sockaddr_in6);
                Some(IpAddr::from(address.sin6_addr.s6_addr))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVERY_INTERFACE_ADDRESS: [&str; 5] = [
        "127.0.0.1",
        "192.168.1.40",
        "169.254.7.7",
        "::1",
        "2001:db8::1",
    ];

    fn every_interface_address() -> Vec<IpAddr> {
        EVERY_INTERFACE_ADDRESS
            .iter()
            .map(|address| address.parse().expect("a literal address"))
            .collect()
    }

    /// A wildcard IPv6 bind covers every routable address on the host, in both
    /// families, and skips loopback and link-local.
    #[test]
    fn a_wildcard_bind_names_every_routable_address_and_no_other() {
        assert_eq!(
            control_plane_urls_for("::", 9000, &every_interface_address()),
            ["http://192.168.1.40:9000", "http://[2001:db8::1]:9000"]
        );
    }

    /// An IPv6 literal is bracketed, which is the difference between a URL a
    /// client can parse and one it cannot.
    #[test]
    fn an_ipv6_address_is_bracketed() {
        assert_eq!(
            control_plane_urls_for("2001:db8::1", 9000, &[]),
            ["http://[2001:db8::1]:9000"]
        );
    }

    /// A wildcard IPv4 bind covers no IPv6 address, because nothing is
    /// listening on one.
    #[test]
    fn a_wildcard_ipv4_bind_names_no_ipv6_address() {
        assert_eq!(
            control_plane_urls_for("0.0.0.0", 9000, &every_interface_address()),
            ["http://192.168.1.40:9000"]
        );
    }

    /// A bind narrowed to loopback is reachable from nowhere else, so it names
    /// nothing rather than a URL no peer can use.
    #[test]
    fn a_loopback_bind_names_nothing() {
        assert!(control_plane_urls_for("127.0.0.1", 9000, &every_interface_address()).is_empty());
    }

    /// A host name is passed through as the caller wrote it: the engine
    /// resolves nothing, and the name is what a caller was told to use.
    #[test]
    fn a_bind_host_that_is_a_name_is_passed_through() {
        assert_eq!(
            control_plane_urls_for("rig.local", 9000, &every_interface_address()),
            ["http://rig.local:9000"]
        );
    }

    /// With no control plane hosted the registry names nothing, and it names
    /// nothing again once one goes.
    #[test]
    fn a_runtime_hosting_no_control_plane_names_no_url() {
        let registry = HostedControlPlaneEndpointRegistry::default();
        assert!(registry.urls_another_machine_could_reach_it_at().is_empty());

        registry.record_what_the_control_plane_bound("127.0.0.1", 9000);
        registry.record_that_the_control_plane_is_gone();
        assert!(registry.urls_another_machine_could_reach_it_at().is_empty());
    }

    /// The host's own interfaces are readable, and every address they report
    /// renders as a URL a client can parse.
    #[test]
    fn this_hosts_own_interfaces_render_as_parseable_urls() {
        for address in every_address_of_this_hosts_interfaces() {
            let rendered = bracketed_if_it_is_a_literal(address);
            assert!(
                address.is_ipv4() || rendered.starts_with('[') && rendered.ends_with(']'),
                "{address} must be bracketed: {rendered}"
            );
        }
    }
}

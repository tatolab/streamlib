// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared iceoryx2 payload types for cross-process IPC communication.
//!
//! Wire format is structured-everywhere (#401 phase 2):
//! [`SchemaIdentWire`] carries `(org, package, type, version)` as separate
//! fixed-width fields rather than a joined string. See
//! `docs/architecture/schema-identity-and-packaging.md` Decision 2.
//!
//! No parser ever runs at the wire boundary: producers obtain structured
//! segments from the build-time `EMBEDDED_SCHEMA_IDENT_SEGMENTS` table in
//! the `streamlib` host crate (or directly from the Surface 2 IPC envelope
//! for cdylibs) and call [`SchemaIdentWire::from_segments`] to materialize
//! the wire bytes.

use iceoryx2::prelude::*;

/// Default hint used to prime a publisher's initial iceoryx2 slot capacity
/// when a wire schema declares no `metadata.expected_payload_bytes`.
///
/// This is a HINT, never a cap. Publishers open under
/// [`iceoryx2::prelude::AllocationStrategy::PowerOfTwo`]; the first loan larger
/// than the primed capacity grows the shared-memory segment and subscribers
/// remap transparently. Sizing the hint to the common-case payload keeps the
/// steady state at a single segment while leaving oversized frames (a first
/// multi-MB keyframe) free to grow rather than crash.
pub const DEFAULT_EXPECTED_PAYLOAD_BYTES: usize = 65536;
pub const MAX_PORT_KEY_SIZE: usize = 64;
pub const MAX_EVENT_PAYLOAD_SIZE: usize = 8192;
pub const MAX_TOPIC_KEY_SIZE: usize = 128;

/// Per-channel payload ceiling for a trusted (in-process host) data channel —
/// the graceful, observable layer in front of the subprocess cgroup
/// `memory.max` hard backstop. A payload above this is refused with a named
/// `PayloadExceedsChannelCeiling` error, counted, and the stream continues;
/// the process never dies.
pub const TRUSTED_CHANNEL_PAYLOAD_CEILING_BYTES: usize = 64 * 1024 * 1024;

/// Per-channel payload ceiling for an untrusted-session (subprocess) data
/// channel. Tighter than the trusted tier because a subprocess payload crosses
/// a trust boundary and a runaway producer must be bounded well below host RAM.
pub const UNTRUSTED_SESSION_CHANNEL_PAYLOAD_CEILING_BYTES: usize = 16 * 1024 * 1024;

/// Trust tier of an iceoryx2 data channel, selecting the default per-channel
/// payload ceiling.
///
/// Determined structurally by the process boundary at wire time: an in-process
/// host link is [`ChannelTrustTier::Trusted`]; a link crossing a subprocess
/// boundary is [`ChannelTrustTier::UntrustedSession`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelTrustTier {
    /// In-process host-to-host channel.
    Trusted,
    /// Channel with a subprocess (Python / Deno) on either end.
    UntrustedSession,
}

impl ChannelTrustTier {
    /// The default per-channel payload ceiling in bytes for this tier.
    pub const fn default_ceiling_bytes(self) -> usize {
        match self {
            Self::Trusted => TRUSTED_CHANNEL_PAYLOAD_CEILING_BYTES,
            Self::UntrustedSession => UNTRUSTED_SESSION_CHANNEL_PAYLOAD_CEILING_BYTES,
        }
    }

    /// Stable lowercase label used in the channel-egress tracing fields.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Trusted => "trusted",
            Self::UntrustedSession => "untrusted-session",
        }
    }
}

/// A PowerOfTwo data-segment growth event a channel publisher observed while
/// admitting a frame: the tracked slot capacity crossed the frame size and was
/// advanced from `old_segment_bytes` to `new_segment_bytes` (`next_power_of_two`).
///
/// `crossed_quarter_ceiling` is `true` when this growth is the one that first
/// pushed the segment past a quarter of the channel's ceiling (`old <= ceiling/4
/// < new`) — the early-warning threshold every runtime raises a `tracing::warn`
/// on. The threshold lives here, alongside the growth bookkeeping, so the host
/// writer and the Python / Deno subprocess natives cannot drift on where it sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelSegmentGrowth {
    /// Tracked slot capacity before this growth, in bytes.
    pub old_segment_bytes: usize,
    /// Tracked slot capacity after this growth (`next_power_of_two`), in bytes.
    pub new_segment_bytes: usize,
    /// Whether this growth first crossed a quarter of the channel ceiling.
    pub crossed_quarter_ceiling: bool,
}

/// Outcome of [`decide_channel_egress_admission`]: whether the frame a channel
/// publisher is about to loan should be published or dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelEgressAdmission {
    /// The frame is above the channel's per-channel payload ceiling and was
    /// refused. The caller drops it — surfacing the refusal in its own way (a
    /// typed `PayloadExceedsChannelCeiling` error in the host, a refuse return
    /// code in a subprocess native) — and logs it; `refused_count` is the
    /// running total after this refusal.
    RefusedOverCeiling { refused_count: u64 },
    /// The frame fits under the ceiling; the caller publishes it. When `grew_to`
    /// is `Some(growth)` the tracked data-segment capacity crossed the frame size
    /// and was advanced — a PowerOfTwo growth the caller logs, additionally
    /// raising a `warn` when [`ChannelSegmentGrowth::crossed_quarter_ceiling`].
    Admitted { grew_to: Option<ChannelSegmentGrowth> },
}

/// Single authority for the per-channel-egress ceiling refusal + PowerOfTwo
/// growth-observability bookkeeping every channel publisher runs before loaning
/// a frame.
///
/// Refusing above `channel_ceiling_bytes` is the graceful, observable layer in
/// front of the subprocess cgroup `memory.max` backstop. This crate owns the
/// thresholds so the host writer and the Python / Deno subprocess natives cannot
/// drift: it increments `refused_over_ceiling_count` on a refusal, advances
/// `current_slot_capacity_bytes` to the next power of two on a growth (both in
/// place), and reports whether that growth first crossed a quarter of the
/// ceiling. The caller owns the tracing and the refusal surface (typed error vs.
/// refuse return code), keeping this wire-types crate logging-free.
pub fn decide_channel_egress_admission(
    frame_total_bytes: usize,
    channel_ceiling_bytes: usize,
    refused_over_ceiling_count: &mut u64,
    current_slot_capacity_bytes: &mut usize,
) -> ChannelEgressAdmission {
    if frame_total_bytes > channel_ceiling_bytes {
        *refused_over_ceiling_count += 1;
        return ChannelEgressAdmission::RefusedOverCeiling {
            refused_count: *refused_over_ceiling_count,
        };
    }
    let grew_to = if frame_total_bytes > *current_slot_capacity_bytes {
        let old_segment_bytes = *current_slot_capacity_bytes;
        let new_segment_bytes = frame_total_bytes.next_power_of_two();
        *current_slot_capacity_bytes = new_segment_bytes;
        let quarter_ceiling_bytes = channel_ceiling_bytes / 4;
        Some(ChannelSegmentGrowth {
            old_segment_bytes,
            new_segment_bytes,
            crossed_quarter_ceiling: new_segment_bytes > quarter_ceiling_bytes
                && old_segment_bytes <= quarter_ceiling_bytes,
        })
    } else {
        None
    };
    ChannelEgressAdmission::Admitted { grew_to }
}

/// Default iceoryx2 ring depth (slot count, not bytes) for the data
/// pub/sub channel between two processors. Wire schemas override this
/// per-vocabulary via `metadata.max_queued_messages` in their YAML.
///
/// iceoryx2 pre-allocates `DEFAULT_MAX_QUEUED_MESSAGES * (primed slot bytes)`
/// of shared memory per publisher when the wire schema does not declare its
/// own depth, so this value is a per-publisher memory commitment too. The slot
/// bytes are primed from [`DEFAULT_EXPECTED_PAYLOAD_BYTES`] (or the schema's
/// `metadata.expected_payload_bytes` hint) and grow on demand.
pub const DEFAULT_MAX_QUEUED_MESSAGES: usize = 16;

/// On-wire size of a [`SchemaIdentWire`]. Held constant at 128 bytes so
/// the total [`FrameHeader`] layout matches the pre-#401-phase-2
/// `SchemaName`-shaped predecessor.
pub const SCHEMA_IDENT_WIRE_SIZE: usize = 128;

/// Maximum byte length of the org segment when serialized into a
/// [`SchemaIdentWire`]. Real-world orgs sit under ~16 chars; 31 leaves
/// room for any plausible org name (GitHub's 39-char org cap is the upper
/// bound of real-world usage).
pub const SCHEMA_IDENT_WIRE_MAX_ORG_LEN: usize = 31;

/// Maximum byte length of the package segment.
pub const SCHEMA_IDENT_WIRE_MAX_PACKAGE_LEN: usize = 31;

/// Maximum byte length of the type-name segment. Wider than org/package
/// because PascalCase processor types like `EncodedVideoFrame` (17) need
/// headroom for any plausible deeply-nested type name; 51 keeps the struct
/// neatly at 128 bytes.
pub const SCHEMA_IDENT_WIRE_MAX_TYPE_LEN: usize = 51;

/// Publishers on a channel-centric iceoryx2 pub/sub data service.
///
/// A channel is keyed on its **source output port** — one source port publishes
/// to exactly one channel — so every data service carries exactly ONE publisher
/// and fans a single zero-copy loan out to its N compile-time-known subscribers.
/// iceoryx2 verifies the publisher count on `open`, so this is pinned in lockstep
/// on the host service builder and both subprocess SDK builders (Python, Deno);
/// a per-runtime divergence would itself break cross-language wiring.
pub const MAX_PUBLISHERS_PER_CHANNEL: usize = 1;

/// Subscriber slots reserved on every channel data service beyond its
/// compile-time-known destination count.
///
/// A channel's data service is created with `max_subscribers = N + this`, where
/// `N` is the number of destinations wired to the source output port at
/// graph-compile time. The reserved slot lets the phase-3.5 `tap` op attach a
/// broadcast consumer as a pure subscriber-add with no service re-open — iceoryx2
/// fixes `max_subscribers` at create time, so the headroom must exist up front.
/// iceoryx2 sizes each publisher's shared-memory data segment as
/// `max_subscribers × (subscriber_max_buffer_size + borrowed) + …`, so this is
/// deliberately 1 (not the iceoryx2 default of 8) to keep the per-channel segment
/// sized to its true consumer count plus one tap.
pub const RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL: usize = 1;

/// Size of the frame header in the `[u8]` slice wire format.
pub const FRAME_HEADER_SIZE: usize = MAX_PORT_KEY_SIZE + SCHEMA_IDENT_WIRE_SIZE + 8 + 4; // 204 bytes

/// Error constructing a [`PortKey`] from a name that overflows the fixed
/// wire capacity.
///
/// This is the engine-layer replacement for the pre-#1416 silent truncation:
/// a port / channel name longer than [`MAX_PORT_KEY_SIZE`] `- 1` bytes used to
/// be quietly clipped, routing frames to a different (truncated) port than the
/// one the author named. Over-length is now a hard, named error the caller
/// must handle rather than a data-corruption surface. Names crossing this
/// boundary have already passed the charset + length grammar in
/// `streamlib_idents::validate_channel_name`; this guard is the wire-level
/// backstop that makes truncation unrepresentable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortKeyError {
    /// The UTF-8 name is `len` bytes, past the fixed `max`-byte capacity.
    TooLong { len: usize, max: usize },
}

impl std::fmt::Display for PortKeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLong { len, max } => write!(
                f,
                "port key name is {len} bytes, exceeding the fixed wire capacity of {max} bytes"
            ),
        }
    }
}

impl std::error::Error for PortKeyError {}

/// Fixed-size port name for zero-copy IPC.
#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug, ZeroCopySend)]
#[repr(C)]
pub struct PortKey {
    len: u8,
    name: [u8; MAX_PORT_KEY_SIZE - 1],
}

impl PortKey {
    /// Maximum UTF-8 byte length a port / channel name may occupy on the wire
    /// (the fixed `name` field is [`MAX_PORT_KEY_SIZE`] `- 1` bytes).
    pub const MAX_NAME_BYTES: usize = MAX_PORT_KEY_SIZE - 1;

    /// Construct a [`PortKey`], rejecting an over-length name.
    ///
    /// A name past [`PortKey::MAX_NAME_BYTES`] is a [`PortKeyError::TooLong`]
    /// rather than a silent truncation — see [`PortKeyError`].
    pub fn new(name: &str) -> Result<Self, PortKeyError> {
        let bytes = name.as_bytes();
        if bytes.len() > Self::MAX_NAME_BYTES {
            return Err(PortKeyError::TooLong {
                len: bytes.len(),
                max: Self::MAX_NAME_BYTES,
            });
        }
        let mut key = Self {
            len: bytes.len() as u8,
            name: [0u8; MAX_PORT_KEY_SIZE - 1],
        };
        key.name[..bytes.len()].copy_from_slice(bytes);
        Ok(key)
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.name[..self.len as usize]).unwrap_or("")
    }
}

impl Default for PortKey {
    fn default() -> Self {
        Self {
            len: 0,
            name: [0u8; MAX_PORT_KEY_SIZE - 1],
        }
    }
}

/// Errors returned when constructing a [`SchemaIdentWire`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaIdentWireError {
    OrgTooLong { len: usize, max: usize },
    PackageTooLong { len: usize, max: usize },
    TypeTooLong { len: usize, max: usize },
}

impl std::fmt::Display for SchemaIdentWireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OrgTooLong { len, max } => {
                write!(f, "schema ident org segment is {len} bytes (max {max})")
            }
            Self::PackageTooLong { len, max } => {
                write!(f, "schema ident package segment is {len} bytes (max {max})")
            }
            Self::TypeTooLong { len, max } => {
                write!(f, "schema ident type segment is {len} bytes (max {max})")
            }
        }
    }
}

impl std::error::Error for SchemaIdentWireError {}

/// Structured schema identifier on the iceoryx2 wire — `@org/package/Type@version`.
///
/// Replaces the joined-string `SchemaName` predecessor (#401 phase 2). The
/// architecture's structured-everywhere rule (Decision 2) requires every
/// wire surface — including iceoryx2 payloads — to carry the four
/// identifier segments as separate fields rather than a single joined
/// string subject to per-runtime parsing drift.
///
/// Layout (`#[repr(C)]`, alignment 4, total 128 bytes):
///
/// ```text
/// offset  0      : org_len: u8
/// offset  1..=31 : org bytes (UTF-8, length=`org_len`)
/// offset 32      : package_len: u8
/// offset 33..=63 : package bytes
/// offset 64      : type_len: u8
/// offset 65..=115: type bytes
/// offset 116..=119: version_major: u32 little-endian
/// offset 120..=123: version_minor: u32 little-endian
/// offset 124..=127: version_patch: u32 little-endian
/// ```
///
/// Endianness: little-endian for the version u32 fields (matches the
/// little-endian `timestamp_ns` and `len` fields elsewhere in
/// [`FrameHeader`]; matches every supported streamlib platform).
///
/// Length-prefix semantics: `*_len = 0` means "empty segment" (zero
/// readable bytes); the trailing buffer bytes are zeroed at construction.
#[derive(Clone, Copy, Eq, PartialEq, Hash, ZeroCopySend)]
#[repr(C)]
pub struct SchemaIdentWire {
    pub org_len: u8,
    pub org: [u8; SCHEMA_IDENT_WIRE_MAX_ORG_LEN],
    pub package_len: u8,
    pub package: [u8; SCHEMA_IDENT_WIRE_MAX_PACKAGE_LEN],
    pub type_len: u8,
    pub type_name: [u8; SCHEMA_IDENT_WIRE_MAX_TYPE_LEN],
    pub version_major: u32,
    pub version_minor: u32,
    pub version_patch: u32,
}

const _: () = {
    // Compile-time ABI lock — drift trips immediately. The whole point of
    // this struct is to be byte-identical across Rust + Python + Deno.
    assert!(std::mem::size_of::<SchemaIdentWire>() == SCHEMA_IDENT_WIRE_SIZE);
    assert!(std::mem::align_of::<SchemaIdentWire>() == 4);
};

impl SchemaIdentWire {
    /// Construct from validated segment strings + version components.
    ///
    /// Performs length-bound validation against the per-segment maxima
    /// (org ≤ 31, package ≤ 31, type ≤ 51 bytes). Charset / grammar
    /// validation is the upstream caller's responsibility — by the time
    /// data reaches the wire format the segments have already been
    /// validated by `streamlib_idents::Org::new` /
    /// `streamlib_idents::Package::new` /
    /// `streamlib_idents::TypeName::new` (or their codegen / build-time
    /// equivalents).
    pub fn from_segments(
        org: &str,
        package: &str,
        type_name: &str,
        version_major: u32,
        version_minor: u32,
        version_patch: u32,
    ) -> Result<Self, SchemaIdentWireError> {
        let org_bytes = org.as_bytes();
        if org_bytes.len() > SCHEMA_IDENT_WIRE_MAX_ORG_LEN {
            return Err(SchemaIdentWireError::OrgTooLong {
                len: org_bytes.len(),
                max: SCHEMA_IDENT_WIRE_MAX_ORG_LEN,
            });
        }
        let package_bytes = package.as_bytes();
        if package_bytes.len() > SCHEMA_IDENT_WIRE_MAX_PACKAGE_LEN {
            return Err(SchemaIdentWireError::PackageTooLong {
                len: package_bytes.len(),
                max: SCHEMA_IDENT_WIRE_MAX_PACKAGE_LEN,
            });
        }
        let type_bytes = type_name.as_bytes();
        if type_bytes.len() > SCHEMA_IDENT_WIRE_MAX_TYPE_LEN {
            return Err(SchemaIdentWireError::TypeTooLong {
                len: type_bytes.len(),
                max: SCHEMA_IDENT_WIRE_MAX_TYPE_LEN,
            });
        }
        let mut wire = Self::default();
        wire.org_len = org_bytes.len() as u8;
        wire.org[..org_bytes.len()].copy_from_slice(org_bytes);
        wire.package_len = package_bytes.len() as u8;
        wire.package[..package_bytes.len()].copy_from_slice(package_bytes);
        wire.type_len = type_bytes.len() as u8;
        wire.type_name[..type_bytes.len()].copy_from_slice(type_bytes);
        wire.version_major = version_major;
        wire.version_minor = version_minor;
        wire.version_patch = version_patch;
        Ok(wire)
    }

    /// Whether this is the zero-segment "unset" wire tag a producer stamps
    /// for a [`PortSchemaSpec::Any`] output — no org / package / type and a
    /// `0.0.0` version. Schema-agreement checks treat an unset tag on either
    /// side as the tolerant wildcard: it never triggers a mismatch.
    ///
    /// [`PortSchemaSpec::Any`]: https://docs.rs/streamlib-processor-schema
    pub fn is_unset(&self) -> bool {
        self.org_len == 0
            && self.package_len == 0
            && self.type_len == 0
            && self.version_major == 0
            && self.version_minor == 0
            && self.version_patch == 0
    }

    pub fn org_str(&self) -> &str {
        std::str::from_utf8(&self.org[..self.org_len as usize]).unwrap_or("")
    }

    pub fn package_str(&self) -> &str {
        std::str::from_utf8(&self.package[..self.package_len as usize]).unwrap_or("")
    }

    pub fn type_str(&self) -> &str {
        std::str::from_utf8(&self.type_name[..self.type_len as usize]).unwrap_or("")
    }

    /// Render the joined `@org/package/Type@major.minor.patch` form for
    /// human-facing surfaces (logs, error messages). One-way: the joined
    /// form never round-trips back through any parser at the structured
    /// boundary (architecture Decision 2). Use the typed `*_str` /
    /// `version_*` accessors for structured access.
    pub fn render_joined(&self) -> String {
        format!(
            "@{}/{}/{}@{}.{}.{}",
            self.org_str(),
            self.package_str(),
            self.type_str(),
            self.version_major,
            self.version_minor,
            self.version_patch,
        )
    }
}

impl Default for SchemaIdentWire {
    fn default() -> Self {
        Self {
            org_len: 0,
            org: [0u8; SCHEMA_IDENT_WIRE_MAX_ORG_LEN],
            package_len: 0,
            package: [0u8; SCHEMA_IDENT_WIRE_MAX_PACKAGE_LEN],
            type_len: 0,
            type_name: [0u8; SCHEMA_IDENT_WIRE_MAX_TYPE_LEN],
            version_major: 0,
            version_minor: 0,
            version_patch: 0,
        }
    }
}

impl std::fmt::Debug for SchemaIdentWire {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SchemaIdentWire")
            .field("org", &self.org_str())
            .field("package", &self.package_str())
            .field("type", &self.type_str())
            .field(
                "version",
                &format_args!(
                    "{}.{}.{}",
                    self.version_major, self.version_minor, self.version_patch
                ),
            )
            .finish()
    }
}

/// Header for slice-based iceoryx2 frame transport.
///
/// Wire format in a `[u8]` slice (little-endian for multi-byte fields):
/// `[port_key: 64][schema_ident: 128][timestamp_ns: 8][len: 4][data: len]`
///
/// The 128-byte `schema_ident` block is a structured [`SchemaIdentWire`]
/// (org/package/type/version, length-prefixed segments + LE u32 versions),
/// not a joined string.
pub struct FrameHeader {
    pub port_key: PortKey,
    pub schema_ident: SchemaIdentWire,
    pub timestamp_ns: i64,
    pub len: u32,
}

impl FrameHeader {
    /// Create a new frame header from a structured schema identifier.
    ///
    /// Fails with [`PortKeyError`] if `port` overflows the fixed wire capacity —
    /// see [`PortKey::new`].
    pub fn new(
        port: &str,
        schema_ident: SchemaIdentWire,
        timestamp_ns: i64,
        data_len: u32,
    ) -> Result<Self, PortKeyError> {
        Ok(Self {
            port_key: PortKey::new(port)?,
            schema_ident,
            timestamp_ns,
            len: data_len,
        })
    }

    /// Write the header to the first [`FRAME_HEADER_SIZE`] bytes of `buf`.
    pub fn write_to_slice(&self, buf: &mut [u8]) {
        // port_key: [len: 1][name: 63] = 64 bytes
        buf[0] = self.port_key.len;
        buf[1..MAX_PORT_KEY_SIZE].copy_from_slice(&self.port_key.name);
        // schema_ident: SchemaIdentWire = 128 bytes (structured, LE u32 versions)
        let s = MAX_PORT_KEY_SIZE;
        write_schema_ident_to_slice(&self.schema_ident, &mut buf[s..s + SCHEMA_IDENT_WIRE_SIZE]);
        // timestamp_ns: 8 bytes little-endian
        let t = s + SCHEMA_IDENT_WIRE_SIZE;
        buf[t..t + 8].copy_from_slice(&self.timestamp_ns.to_le_bytes());
        // len: 4 bytes little-endian
        buf[t + 8..t + 12].copy_from_slice(&self.len.to_le_bytes());
    }

    /// Read a header from the first [`FRAME_HEADER_SIZE`] bytes of `buf`.
    pub fn read_from_slice(buf: &[u8]) -> Self {
        let mut port_key = PortKey::default();
        port_key.len = buf[0];
        port_key.name.copy_from_slice(&buf[1..MAX_PORT_KEY_SIZE]);

        let s = MAX_PORT_KEY_SIZE;
        let schema_ident = read_schema_ident_from_slice(&buf[s..s + SCHEMA_IDENT_WIRE_SIZE]);

        let t = s + SCHEMA_IDENT_WIRE_SIZE;
        let timestamp_ns = i64::from_le_bytes(buf[t..t + 8].try_into().unwrap());
        let len = u32::from_le_bytes(buf[t + 8..t + 12].try_into().unwrap());

        Self {
            port_key,
            schema_ident,
            timestamp_ns,
            len,
        }
    }

    /// Read the port key string from a raw slice without parsing the full header.
    pub fn read_port_from_slice(buf: &[u8]) -> &str {
        let len = buf[0] as usize;
        std::str::from_utf8(&buf[1..1 + len]).unwrap_or("")
    }

    /// Get the port key as a string.
    pub fn port(&self) -> &str {
        self.port_key.as_str()
    }

    /// Get the structured schema identifier.
    pub fn schema(&self) -> &SchemaIdentWire {
        &self.schema_ident
    }
}

/// Write a [`SchemaIdentWire`] to the first [`SCHEMA_IDENT_WIRE_SIZE`] bytes
/// of `buf` (little-endian for the version u32 fields).
fn write_schema_ident_to_slice(ident: &SchemaIdentWire, buf: &mut [u8]) {
    debug_assert!(buf.len() >= SCHEMA_IDENT_WIRE_SIZE);
    buf[0] = ident.org_len;
    buf[1..1 + SCHEMA_IDENT_WIRE_MAX_ORG_LEN].copy_from_slice(&ident.org);
    let p = 1 + SCHEMA_IDENT_WIRE_MAX_ORG_LEN; // 32
    buf[p] = ident.package_len;
    buf[p + 1..p + 1 + SCHEMA_IDENT_WIRE_MAX_PACKAGE_LEN].copy_from_slice(&ident.package);
    let t = p + 1 + SCHEMA_IDENT_WIRE_MAX_PACKAGE_LEN; // 64
    buf[t] = ident.type_len;
    buf[t + 1..t + 1 + SCHEMA_IDENT_WIRE_MAX_TYPE_LEN].copy_from_slice(&ident.type_name);
    let v = t + 1 + SCHEMA_IDENT_WIRE_MAX_TYPE_LEN; // 116
    buf[v..v + 4].copy_from_slice(&ident.version_major.to_le_bytes());
    buf[v + 4..v + 8].copy_from_slice(&ident.version_minor.to_le_bytes());
    buf[v + 8..v + 12].copy_from_slice(&ident.version_patch.to_le_bytes());
}

fn read_schema_ident_from_slice(buf: &[u8]) -> SchemaIdentWire {
    debug_assert!(buf.len() >= SCHEMA_IDENT_WIRE_SIZE);
    let mut ident = SchemaIdentWire::default();
    ident.org_len = buf[0];
    ident
        .org
        .copy_from_slice(&buf[1..1 + SCHEMA_IDENT_WIRE_MAX_ORG_LEN]);
    let p = 1 + SCHEMA_IDENT_WIRE_MAX_ORG_LEN;
    ident.package_len = buf[p];
    ident
        .package
        .copy_from_slice(&buf[p + 1..p + 1 + SCHEMA_IDENT_WIRE_MAX_PACKAGE_LEN]);
    let t = p + 1 + SCHEMA_IDENT_WIRE_MAX_PACKAGE_LEN;
    ident.type_len = buf[t];
    ident
        .type_name
        .copy_from_slice(&buf[t + 1..t + 1 + SCHEMA_IDENT_WIRE_MAX_TYPE_LEN]);
    let v = t + 1 + SCHEMA_IDENT_WIRE_MAX_TYPE_LEN;
    ident.version_major = u32::from_le_bytes(buf[v..v + 4].try_into().unwrap());
    ident.version_minor = u32::from_le_bytes(buf[v + 4..v + 8].try_into().unwrap());
    ident.version_patch = u32::from_le_bytes(buf[v + 8..v + 12].try_into().unwrap());
    ident
}

/// Fixed-size topic name for event pub/sub IPC.
#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug, ZeroCopySend)]
#[repr(C)]
pub struct TopicKey {
    len: u8,
    name: [u8; MAX_TOPIC_KEY_SIZE - 1],
}

impl TopicKey {
    pub fn new(name: &str) -> Self {
        let bytes = name.as_bytes();
        let len = bytes.len().min(MAX_TOPIC_KEY_SIZE - 1) as u8;
        let mut key = Self {
            len,
            name: [0u8; MAX_TOPIC_KEY_SIZE - 1],
        };
        key.name[..len as usize].copy_from_slice(&bytes[..len as usize]);
        key
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.name[..self.len as usize]).unwrap_or("")
    }
}

impl Default for TopicKey {
    fn default() -> Self {
        Self {
            len: 0,
            name: [0u8; MAX_TOPIC_KEY_SIZE - 1],
        }
    }
}

/// Event payload for iceoryx2 pub/sub communication.
///
/// Carries serialized runtime events (lifecycle, graph changes, compiler, input)
/// between components via iceoryx2 shared memory.
#[derive(Clone, Copy, ZeroCopySend)]
#[type_name("EventPayload")]
#[repr(C)]
pub struct EventPayload {
    pub topic_key: TopicKey,
    pub timestamp_ns: i64,
    pub len: u32,
    pub data: [u8; MAX_EVENT_PAYLOAD_SIZE],
}

impl EventPayload {
    /// Create a new event payload with the given topic and serialized data.
    pub fn new(topic: &str, timestamp_ns: i64, data: &[u8]) -> Self {
        let len = data.len().min(MAX_EVENT_PAYLOAD_SIZE) as u32;
        let mut payload = Self {
            topic_key: TopicKey::new(topic),
            timestamp_ns,
            len,
            data: [0u8; MAX_EVENT_PAYLOAD_SIZE],
        };
        payload.data[..len as usize].copy_from_slice(&data[..len as usize]);
        payload
    }

    /// Get the actual data slice (excluding padding).
    pub fn data(&self) -> &[u8] {
        &self.data[..self.len as usize]
    }

    /// Get the topic key as a string.
    pub fn topic(&self) -> &str {
        self.topic_key.as_str()
    }
}

impl Default for EventPayload {
    fn default() -> Self {
        Self {
            topic_key: TopicKey::default(),
            timestamp_ns: 0,
            len: 0,
            data: [0u8; MAX_EVENT_PAYLOAD_SIZE],
        }
    }
}

impl std::fmt::Debug for EventPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventPayload")
            .field("topic_key", &self.topic_key.as_str())
            .field("timestamp_ns", &self.timestamp_ns)
            .field("len", &self.len)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_ident() -> SchemaIdentWire {
        SchemaIdentWire::from_segments("tatolab", "core", "VideoFrame", 1, 0, 0).unwrap()
    }

    #[test]
    fn schema_ident_wire_layout_locked() {
        // ABI lock — these constants are part of the cross-runtime contract.
        // Drift between Rust + Python ctypes + Deno FFI tripped immediately
        // here means the const_assert above will already have failed.
        assert_eq!(std::mem::size_of::<SchemaIdentWire>(), 128);
        assert_eq!(std::mem::align_of::<SchemaIdentWire>(), 4);
    }

    #[test]
    fn schema_ident_wire_round_trip_struct_to_struct() {
        let ident = sample_ident();
        assert_eq!(ident.org_str(), "tatolab");
        assert_eq!(ident.package_str(), "core");
        assert_eq!(ident.type_str(), "VideoFrame");
        assert_eq!(ident.version_major, 1);
        assert_eq!(ident.version_minor, 0);
        assert_eq!(ident.version_patch, 0);
        assert_eq!(ident.render_joined(), "@tatolab/core/VideoFrame@1.0.0");
    }

    #[test]
    fn schema_ident_wire_round_trip_via_slice() {
        let ident = sample_ident();
        let mut buf = [0u8; SCHEMA_IDENT_WIRE_SIZE];
        write_schema_ident_to_slice(&ident, &mut buf);
        let back = read_schema_ident_from_slice(&buf);
        assert_eq!(ident, back);
        assert_eq!(back.render_joined(), "@tatolab/core/VideoFrame@1.0.0");
    }

    #[test]
    fn schema_ident_wire_rejects_oversized_segments() {
        let too_long_org = "a".repeat(SCHEMA_IDENT_WIRE_MAX_ORG_LEN + 1);
        assert!(matches!(
            SchemaIdentWire::from_segments(&too_long_org, "core", "VideoFrame", 1, 0, 0),
            Err(SchemaIdentWireError::OrgTooLong { .. })
        ));
        let too_long_pkg = "a".repeat(SCHEMA_IDENT_WIRE_MAX_PACKAGE_LEN + 1);
        assert!(matches!(
            SchemaIdentWire::from_segments("tatolab", &too_long_pkg, "VideoFrame", 1, 0, 0),
            Err(SchemaIdentWireError::PackageTooLong { .. })
        ));
        let too_long_type = "A".repeat(SCHEMA_IDENT_WIRE_MAX_TYPE_LEN + 1);
        assert!(matches!(
            SchemaIdentWire::from_segments("tatolab", "core", &too_long_type, 1, 0, 0),
            Err(SchemaIdentWireError::TypeTooLong { .. })
        ));
    }

    #[test]
    fn frame_header_round_trip_via_slice() {
        let ident = SchemaIdentWire::from_segments("tatolab", "core", "EncodedVideoFrame", 1, 2, 3)
            .unwrap();
        let header = FrameHeader::new("dest_port", ident, 42, 1024).unwrap();
        let mut buf = [0u8; FRAME_HEADER_SIZE];
        header.write_to_slice(&mut buf);
        let back = FrameHeader::read_from_slice(&buf);
        assert_eq!(back.port(), "dest_port");
        assert_eq!(back.schema(), &ident);
        assert_eq!(back.timestamp_ns, 42);
        assert_eq!(back.len, 1024);
        assert_eq!(
            back.schema().render_joined(),
            "@tatolab/core/EncodedVideoFrame@1.2.3"
        );
    }

    #[test]
    fn frame_header_size_matches_constant() {
        // [PortKey: 64][SchemaIdentWire: 128][i64: 8][u32: 4] = 204 bytes.
        assert_eq!(FRAME_HEADER_SIZE, 64 + 128 + 8 + 4);
        assert_eq!(FRAME_HEADER_SIZE, 204);
    }

    #[test]
    fn channel_trust_tier_defaults_and_labels() {
        assert_eq!(
            ChannelTrustTier::Trusted.default_ceiling_bytes(),
            TRUSTED_CHANNEL_PAYLOAD_CEILING_BYTES
        );
        assert_eq!(
            ChannelTrustTier::UntrustedSession.default_ceiling_bytes(),
            UNTRUSTED_SESSION_CHANNEL_PAYLOAD_CEILING_BYTES
        );
        assert!(
            ChannelTrustTier::UntrustedSession.default_ceiling_bytes()
                < ChannelTrustTier::Trusted.default_ceiling_bytes(),
            "untrusted-session ceiling must be tighter than trusted"
        );
        assert_eq!(ChannelTrustTier::Trusted.as_str(), "trusted");
        assert_eq!(
            ChannelTrustTier::UntrustedSession.as_str(),
            "untrusted-session"
        );
    }

    #[test]
    fn schema_ident_wire_max_segment_lengths() {
        // Boundary values — exact-fit segments must succeed.
        let max_org = "a".repeat(SCHEMA_IDENT_WIRE_MAX_ORG_LEN);
        let max_pkg = "b".repeat(SCHEMA_IDENT_WIRE_MAX_PACKAGE_LEN);
        let max_type = "C".repeat(SCHEMA_IDENT_WIRE_MAX_TYPE_LEN);
        let ident = SchemaIdentWire::from_segments(
            &max_org,
            &max_pkg,
            &max_type,
            u32::MAX,
            u32::MAX,
            u32::MAX,
        )
        .unwrap();
        assert_eq!(ident.org_str(), max_org);
        assert_eq!(ident.package_str(), max_pkg);
        assert_eq!(ident.type_str(), max_type);
        assert_eq!(ident.version_major, u32::MAX);
    }

    #[test]
    fn schema_ident_wire_offsets_match_documented_layout() {
        // Fixed-offset assertions — these are part of the documented wire
        // format that Python ctypes and Deno FFI mirror. If the Rust layout
        // shifts (e.g. someone reorders fields, or alignment padding is
        // inserted) this test catches it.
        let ident = SchemaIdentWire::from_segments("a", "b", "C", 1, 2, 3).unwrap();
        let mut buf = [0u8; SCHEMA_IDENT_WIRE_SIZE];
        write_schema_ident_to_slice(&ident, &mut buf);

        assert_eq!(buf[0], 1, "org_len at offset 0");
        assert_eq!(buf[1], b'a', "org bytes start at offset 1");
        assert_eq!(buf[32], 1, "package_len at offset 32");
        assert_eq!(buf[33], b'b', "package bytes start at offset 33");
        assert_eq!(buf[64], 1, "type_len at offset 64");
        assert_eq!(buf[65], b'C', "type bytes start at offset 65");

        // version u32s little-endian at offsets 116/120/124.
        assert_eq!(u32::from_le_bytes(buf[116..120].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(buf[120..124].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(buf[124..128].try_into().unwrap()), 3);
    }

    #[test]
    fn port_key_accepts_max_length_name() {
        // Exact-fit boundary: a name of MAX_NAME_BYTES must construct.
        let name = "a".repeat(PortKey::MAX_NAME_BYTES);
        let key = PortKey::new(&name).expect("max-length name must construct");
        assert_eq!(key.as_str(), name);
    }

    #[test]
    fn port_key_rejects_over_length_name_instead_of_truncating() {
        // Mental-revert guard for the pre-#1416 silent truncation: a name one
        // byte past the wire capacity must be a named error, NOT a clipped key
        // that routes frames to the wrong port. Revert `PortKey::new` to the
        // `.min(MAX_PORT_KEY_SIZE - 1)` truncation and this fails — the
        // construction would succeed and `as_str()` would return the clipped
        // 63-byte prefix.
        let over = "b".repeat(PortKey::MAX_NAME_BYTES + 1);
        assert_eq!(over.len(), 64);
        assert_eq!(
            PortKey::new(&over),
            Err(PortKeyError::TooLong { len: 64, max: 63 })
        );
    }

    #[test]
    fn egress_admission_refuses_over_ceiling_and_counts() {
        let ceiling = 128 * 1024usize;
        let mut refused = 0u64;
        let mut slot = 64usize;
        // First over-ceiling frame: refused, count → 1, slot untouched.
        assert_eq!(
            decide_channel_egress_admission(ceiling + 1, ceiling, &mut refused, &mut slot),
            ChannelEgressAdmission::RefusedOverCeiling { refused_count: 1 }
        );
        assert_eq!(slot, 64, "a refusal must not grow the tracked slot");
        // Second over-ceiling frame: count keeps climbing.
        assert_eq!(
            decide_channel_egress_admission(ceiling + 999, ceiling, &mut refused, &mut slot),
            ChannelEgressAdmission::RefusedOverCeiling { refused_count: 2 }
        );
    }

    #[test]
    fn egress_admission_grows_without_crossing_quarter_ceiling() {
        let ceiling = 128 * 1024usize; // quarter = 32 KiB
        let mut refused = 0u64;
        let mut slot = 4096usize;
        // A frame that grows the slot but stays at or below the quarter ceiling
        // (32 KiB) must NOT flag a crossing. 20_000 → next_pow2 = 32_768 == quarter.
        match decide_channel_egress_admission(20_000, ceiling, &mut refused, &mut slot) {
            ChannelEgressAdmission::Admitted {
                grew_to: Some(growth),
            } => {
                assert_eq!(growth.old_segment_bytes, 4096);
                assert_eq!(growth.new_segment_bytes, 32_768);
                assert!(
                    !growth.crossed_quarter_ceiling,
                    "new == ceiling/4 is not yet past the quarter — must not warn"
                );
            }
            other => panic!("expected an Admitted growth, got {other:?}"),
        }
        assert_eq!(slot, 32_768, "the slot advances to next_power_of_two");
        assert_eq!(refused, 0);
    }

    #[test]
    fn egress_admission_flags_the_growth_that_crosses_quarter_ceiling() {
        // Mental-revert guard for the quarter-ceiling early warning: this is the
        // single authority the host writer + Python/Deno natives all read the
        // `crossed_quarter_ceiling` flag from, so the threshold can't drift across
        // the three call sites. Drop the `> quarter && old <= quarter` computation
        // and this crossing goes unflagged — no runtime raises the warn.
        let ceiling = 128 * 1024usize; // quarter = 32 KiB = 32_768
        let mut refused = 0u64;
        let mut slot = 4096usize;
        // 40_000 → next_pow2 = 65_536, which is past the 32_768 quarter while the
        // old 4096 slot was under it: exactly the first crossing.
        match decide_channel_egress_admission(40_000, ceiling, &mut refused, &mut slot) {
            ChannelEgressAdmission::Admitted {
                grew_to: Some(growth),
            } => {
                assert_eq!(growth.old_segment_bytes, 4096);
                assert_eq!(growth.new_segment_bytes, 65_536);
                assert!(
                    growth.crossed_quarter_ceiling,
                    "old <= ceiling/4 < new must flag the quarter-ceiling crossing"
                );
            }
            other => panic!("expected an Admitted growth, got {other:?}"),
        }

        // A subsequent still-larger growth does NOT re-flag — the segment already
        // sits past the quarter, so only the FIRST crossing warns.
        match decide_channel_egress_admission(100_000, ceiling, &mut refused, &mut slot) {
            ChannelEgressAdmission::Admitted {
                grew_to: Some(growth),
            } => assert!(
                !growth.crossed_quarter_ceiling,
                "a growth already above the quarter must not re-flag"
            ),
            other => panic!("expected an Admitted growth, got {other:?}"),
        }
    }

    #[test]
    fn egress_admission_admits_within_slot_without_growth() {
        let ceiling = 128 * 1024usize;
        let mut refused = 0u64;
        let mut slot = 65_536usize;
        // A frame at or under the tracked slot neither grows nor flags.
        assert_eq!(
            decide_channel_egress_admission(4096, ceiling, &mut refused, &mut slot),
            ChannelEgressAdmission::Admitted { grew_to: None }
        );
        assert_eq!(slot, 65_536, "an in-slot frame leaves the tracked slot as-is");
        assert_eq!(refused, 0);
    }

    #[test]
    fn frame_header_rejects_over_length_port() {
        // The truncation defect surfaced through FrameHeader::new on the write
        // path — over-length must propagate as the typed error, not silently
        // build a header with a clipped port key.
        let ident = sample_ident();
        let over = "c".repeat(PortKey::MAX_NAME_BYTES + 1);
        assert!(matches!(
            FrameHeader::new(&over, ident, 0, 0),
            Err(PortKeyError::TooLong { .. })
        ));
    }

}

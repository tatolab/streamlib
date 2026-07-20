// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Channel names — the pub/sub rendezvous identifier a source output port
//! publishes to and every downstream input port subscribes from.
//!
//! A channel is keyed on the **source output port** (`{source_processor_id}/{source_output_port}`),
//! not on the link: one source output port maps to exactly one channel, so a
//! single publisher fans out one zero-copy loan to N compile-time-known
//! subscribers regardless of how many destinations it feeds. A `connect()` link
//! is the degenerate 1:1 case of that keying.
//!
//! The name is the same string in two roles: an iceoryx2 service name intra-node
//! and (phase [L]) a Zenoh key-expression cross-node. It is `/`-separated into
//! chunks; each chunk obeys the lowercase-leading ident grammar plus `_`
//! (`[a-z][a-z0-9_-]*`) — the org/package charset widened by underscore so port
//! names like `video_in` cross intact. The `/` is a chunk separator (a Zenoh
//! keyexpr segment boundary), never a within-chunk character. Underscore and
//! hyphen are transport-legal: iceoryx2 `ServiceName` imposes no charset
//! restriction beyond non-empty / length / no `iox2://` prefix, and a Zenoh
//! keyexpr segment forbids only `/ * $ ? #`. A leading `@` chunk is forbidden
//! (Zenoh reserved for admin space) — the per-chunk `[a-z]`-leading rule already
//! excludes it. This module is the single source of truth for that grammar; the
//! SDK and the engine both validate through it rather than forking a parallel
//! copy.
//!
//! # Non-injective single-`-` fold retired
//!
//! The predecessor folded `{src}-{out}--{dst}-{in}` with single hyphens, which
//! is non-injective: `cam-x` + output `out` and `cam` + output `x-out` both
//! render `cam-x-out`. The `/`-separator between the processor-id chunk and the
//! port chunk makes the mapping injective — two distinct `(processor, port)`
//! pairs can never collide onto one channel.
//!
//! The wire carries a channel name through a fixed-width `PortKey`
//! ([`MAX_CHANNEL_NAME_BYTES`] bytes). An over-length explicit name is a hard
//! error; a generated name hash-legalizes its machine-generated chunk (the
//! processor id) in place to stay in bound, never prefix-truncating across a
//! `/` (which would collide two channels).

use crate::error::{IdentError, IdentResult};
use crate::ident::{is_lower_alnum_hyphen_or_underscore, validate_lower_hyphen_grammar};
use std::fmt;

/// Maximum channel-name length in UTF-8 bytes.
///
/// Pinned to the fixed `PortKey` wire capacity
/// (`streamlib_ipc_types::PortKey::MAX_NAME_BYTES`). The engine holds a
/// cross-crate assertion that the two constants agree — this crate has no
/// `streamlib-ipc-types` dependency (that crate pulls in iceoryx2), so the
/// bound is duplicated here as a plain constant and reconciled at the engine
/// layer that depends on both.
pub const MAX_CHANNEL_NAME_BYTES: usize = 63;

/// The chunk separator — a Zenoh keyexpr segment boundary. Chunks on either
/// side obey the per-chunk grammar; the separator itself is never a within-chunk
/// character.
pub const CHANNEL_CHUNK_SEPARATOR: char = '/';

/// Number of lowercase-hex characters in the deterministic disambiguating
/// suffix appended when a generated channel chunk would overflow the budget.
/// 12 hex chars = 48 bits of the chunk hash.
const CHANNEL_NAME_HASH_SUFFIX_HEX_LEN: usize = 12;

/// A validated channel name.
///
/// Constructed via [`ChannelName::new`] (validating an explicit user-supplied
/// name) or [`source_channel_name`] (deterministically generating the name a
/// source output port publishes to). Both paths guarantee the per-chunk charset
/// grammar and the [`MAX_CHANNEL_NAME_BYTES`] bound hold.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChannelName(String);

impl ChannelName {
    /// Validate and wrap an explicit, user-supplied channel name.
    ///
    /// An over-length name is [`IdentError::ChannelNameTooLong`] — never
    /// truncated. Charset violations surface as the matching named variant.
    pub fn new(s: impl Into<String>) -> IdentResult<Self> {
        let s = s.into();
        validate_channel_name(&s)?;
        Ok(Self(s))
    }

    /// Borrow the validated channel name as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the wrapper and return the owned validated channel name.
    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for ChannelName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Validate one `/`-separated chunk's charset grammar (`[a-z][a-z0-9_-]*`)
/// without any length bound. Underscore is admitted (but not for org/package)
/// so a port name like `video_in` is a legal chunk. A `/` inside `s` is itself
/// an invalid character here — callers split on `/` before validating chunks.
fn validate_channel_chunk_charset(s: &str) -> IdentResult<()> {
    validate_lower_hyphen_grammar(
        s,
        is_lower_alnum_hyphen_or_underscore,
        || IdentError::EmptyChannelName,
        |s| IdentError::ChannelNameMustStartWithLowercase(s.to_string()),
        |s, c| IdentError::InvalidChannelNameCharacter(s.to_string(), c),
    )
}

/// Validate every `/`-separated chunk of `s` against the per-chunk charset
/// grammar. An empty chunk (a leading, trailing, or doubled `/`) surfaces as
/// [`IdentError::EmptyChannelName`]. The whole-name length bound is applied
/// separately by [`validate_channel_name`].
fn validate_channel_chunks(s: &str) -> IdentResult<()> {
    if s.is_empty() {
        return Err(IdentError::EmptyChannelName);
    }
    for chunk in s.split(CHANNEL_CHUNK_SEPARATOR) {
        validate_channel_chunk_charset(chunk)?;
    }
    Ok(())
}

/// Validate a channel name against the canonical grammar: one or more
/// `/`-separated chunks, each `[a-z][a-z0-9_-]*`, at most
/// [`MAX_CHANNEL_NAME_BYTES`] UTF-8 bytes total. A leading `@` chunk (Zenoh
/// admin space) and the Zenoh-reserved wildcard characters `* $ ? #` are
/// excluded by the per-chunk charset.
pub fn validate_channel_name(s: &str) -> IdentResult<()> {
    if s.len() > MAX_CHANNEL_NAME_BYTES {
        return Err(IdentError::ChannelNameTooLong {
            name: s.to_string(),
            len: s.len(),
            max: MAX_CHANNEL_NAME_BYTES,
        });
    }
    validate_channel_chunks(s)
}

/// Deterministically derive the channel name a source output port publishes to:
/// `{source_processor_id}/{source_output_port}`.
///
/// Channel identity keys on the **source** only — the same source output port
/// yields the same channel regardless of how many destinations `connect()` to
/// it, which is what lets one publisher fan out a single zero-copy loan to N
/// subscribers. Two `connect()` links from one output share this one channel.
///
/// The processor id is engine-generated (`ProcessorUniqueId` is `P{cuid2}` — an
/// uppercase-leading `P` over a lowercase base-36 body), so its raw form is
/// never lowercase-leading-legal; it is normalized to lowercase before the `/`.
/// The output port name is author-supplied and is NOT normalized: a genuinely
/// illegal character (uppercase, `.`, whitespace, a stray `/`) surfaces as the
/// matching [`IdentError`] charset variant rather than a silently-invalid wire
/// name. Underscore rides through (`video_out` → `…/video_out`).
///
/// If the joined form overflows [`MAX_CHANNEL_NAME_BYTES`], the machine-generated
/// processor-id chunk is shortened and a stable hash of its full form is
/// appended (`{prefix}-{hash}`) — a pure function of the inputs that stays
/// unique. The author-supplied port chunk is never shortened; if the port chunk
/// alone leaves no room for even a hashed processor chunk, the port name is
/// [`IdentError::ChannelNameTooLong`].
pub fn source_channel_name(
    source_processor: &str,
    source_output: &str,
) -> IdentResult<ChannelName> {
    let processor = source_processor.to_ascii_lowercase();
    validate_channel_chunk_charset(&processor)?;
    validate_channel_chunk_charset(source_output)?;

    let sep_len = CHANNEL_CHUNK_SEPARATOR.len_utf8();
    if processor.len() + sep_len + source_output.len() <= MAX_CHANNEL_NAME_BYTES {
        let name = ChannelName(format!(
            "{processor}{CHANNEL_CHUNK_SEPARATOR}{source_output}"
        ));
        debug_assert!(validate_channel_name(name.as_str()).is_ok());
        return Ok(name);
    }

    // Overflow: hash-legalize ONLY the machine-generated processor chunk, never
    // across the `/`. The author-supplied port chunk rides through whole.
    let processor_budget = MAX_CHANNEL_NAME_BYTES
        .checked_sub(sep_len + source_output.len())
        .filter(|budget| *budget >= CHANNEL_NAME_HASH_SUFFIX_HEX_LEN + 1)
        .ok_or_else(|| IdentError::ChannelNameTooLong {
            name: source_output.to_string(),
            len: source_output.len(),
            max: MAX_CHANNEL_NAME_BYTES - sep_len - (CHANNEL_NAME_HASH_SUFFIX_HEX_LEN + 1),
        })?;

    let processor_chunk = hash_legalize_chunk(&processor, processor_budget);
    let name = ChannelName(format!(
        "{processor_chunk}{CHANNEL_CHUNK_SEPARATOR}{source_output}"
    ));
    debug_assert!(validate_channel_name(name.as_str()).is_ok());
    Ok(name)
}

/// Shorten one chunk to fit `budget` bytes while staying a grammar-legal chunk
/// and a pure function of the full chunk: keep as much of the human-readable
/// prefix as fits alongside a `-`-joined stable hash suffix. `budget` is
/// guaranteed by the caller to be at least `CHANNEL_NAME_HASH_SUFFIX_HEX_LEN + 1`.
fn hash_legalize_chunk(chunk: &str, budget: usize) -> String {
    if chunk.len() <= budget {
        return chunk.to_string();
    }
    let hash = fnv1a_64(chunk.as_bytes());
    let suffix = format!("{hash:016x}");
    let suffix = &suffix[suffix.len() - CHANNEL_NAME_HASH_SUFFIX_HEX_LEN..];

    let prefix_budget = budget - 1 - CHANNEL_NAME_HASH_SUFFIX_HEX_LEN;
    let mut cut = prefix_budget.min(chunk.len());
    while cut > 0 && !chunk.is_char_boundary(cut) {
        cut -= 1;
    }
    let prefix = &chunk[..cut];
    format!("{prefix}-{suffix}")
}

/// FNV-1a 64-bit — a fixed, platform-stable hash so a regenerated channel name
/// is byte-identical across builds and runtimes (`std`'s `DefaultHasher` is
/// explicitly not stable across versions).
fn fnv1a_64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x00000100000001b3;
    let mut hash = OFFSET_BASIS;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_canonical_names() {
        for name in ["a", "camera-out", "cam/frame", "proc/video_in", "a1/b2"] {
            validate_channel_name(name).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(ChannelName::new(name).unwrap().as_str(), name);
        }
    }

    #[test]
    fn rejects_empty() {
        assert!(matches!(
            validate_channel_name(""),
            Err(IdentError::EmptyChannelName)
        ));
    }

    #[test]
    fn rejects_empty_chunk_from_slash_boundary() {
        // A leading, trailing, or doubled `/` produces an empty chunk — an
        // illegal channel name, never silently accepted.
        for name in ["/cam", "cam/", "cam//out"] {
            assert!(
                matches!(
                    validate_channel_name(name),
                    Err(IdentError::EmptyChannelName)
                ),
                "{name} must reject as an empty chunk"
            );
        }
    }

    #[test]
    fn rejects_uppercase_and_leading_non_alpha() {
        assert!(matches!(
            validate_channel_name("Camera"),
            Err(IdentError::ChannelNameMustStartWithLowercase(_))
        ));
        assert!(matches!(
            validate_channel_name("1cam"),
            Err(IdentError::ChannelNameMustStartWithLowercase(_))
        ));
        assert!(matches!(
            validate_channel_name("-cam"),
            Err(IdentError::ChannelNameMustStartWithLowercase(_))
        ));
        // Per-chunk: the second chunk must also start lowercase-alpha.
        assert!(matches!(
            validate_channel_name("cam/1out"),
            Err(IdentError::ChannelNameMustStartWithLowercase(_))
        ));
    }

    #[test]
    fn rejects_zenoh_reserved_and_illegal_charset() {
        // Dot, space, and the Zenoh-reserved wildcard/pipeline chars `* $ ? #`
        // are none of them chunk-legal. Underscore and hyphen are NOT in this
        // list — they are transport-legal within a chunk.
        for (name, bad) in [
            ("cam.out", '.'),
            ("cam out", ' '),
            ("cam*", '*'),
            ("cam$out", '$'),
            ("cam?", '?'),
            ("cam#x", '#'),
        ] {
            assert_eq!(
                validate_channel_name(name),
                Err(IdentError::InvalidChannelNameCharacter(name.to_string(), bad))
            );
        }
    }

    #[test]
    fn rejects_leading_at_chunk() {
        // A leading `@` chunk is Zenoh admin space — the per-chunk
        // lowercase-alpha-leading rule excludes it. Mental-revert guard: relax
        // the chunk-leading rule and this stops erroring.
        assert!(matches!(
            validate_channel_name("@admin/thing"),
            Err(IdentError::ChannelNameMustStartWithLowercase(_))
        ));
    }

    #[test]
    fn underscore_is_legal_within_a_chunk() {
        // Underscore is transport-legal, so a shipped underscore port name is a
        // valid chunk. Mental-revert guard: narrow the chunk charset back to
        // `[a-z0-9-]` and this fails.
        for name in ["video_in", "proc/video_out", "proc/encoded_jpeg_in"] {
            validate_channel_name(name).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(ChannelName::new(name).unwrap().as_str(), name);
        }
    }

    #[test]
    fn explicit_over_length_name_is_a_hard_error_not_truncated() {
        // Mental-revert guard for the whole grammar decision: an explicit
        // user-supplied name past the wire bound must error, never truncate.
        let long = "a".repeat(MAX_CHANNEL_NAME_BYTES + 1);
        assert_eq!(long.len(), 64);
        assert_eq!(
            ChannelName::new(&long),
            Err(IdentError::ChannelNameTooLong {
                name: long.clone(),
                len: 64,
                max: MAX_CHANNEL_NAME_BYTES,
            })
        );
    }

    #[test]
    fn exact_bound_name_is_accepted() {
        let exact = "a".repeat(MAX_CHANNEL_NAME_BYTES);
        assert_eq!(exact.len(), MAX_CHANNEL_NAME_BYTES);
        assert!(ChannelName::new(&exact).is_ok());
    }

    #[test]
    fn source_channel_name_is_deterministic_and_source_shaped() {
        let a = source_channel_name("proc", "frame").unwrap();
        let b = source_channel_name("proc", "frame").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.as_str(), "proc/frame");
    }

    #[test]
    fn channel_identity_keys_on_source_only() {
        // The load-bearing D1 property: channel identity is a pure function of
        // the SOURCE output port. The derivation takes no destination argument,
        // so every link out of one source output port — regardless of which
        // destination processor / input port it feeds — resolves to the ONE
        // channel a single publisher fans out over (N subscribers, one loan).
        //
        // The retired per-link fold took `(src, out, dst, in)` and produced a
        // DISTINCT name per destination — which is exactly the N-publisher
        // shape that forced the fan-out copy loop. Keying on `(src, out)` alone
        // is what dissolves it. Same source port ⇒ same channel; the channel
        // moves only when the source port does.
        let a = source_channel_name("cam", "frame").unwrap();
        let same_source = source_channel_name("cam", "frame").unwrap();
        let other_port = source_channel_name("cam", "thumbnail").unwrap();
        let other_proc = source_channel_name("cam2", "frame").unwrap();
        assert_eq!(a, same_source, "one source output port ⇒ exactly one channel");
        assert_ne!(a, other_port, "a different source output port is a different channel");
        assert_ne!(a, other_proc, "a different source processor is a different channel");
    }

    #[test]
    fn distinct_source_ports_stay_distinct() {
        // Injectivity across the `/`: distinct (processor, port) pairs must
        // never collide. Directly exercises the non-injective single-`-` fold
        // defect the `/`-separator fixes — `cam-x` + `out` vs `cam` + `x-out`.
        let a = source_channel_name("camx", "out").unwrap();
        let b = source_channel_name("cam", "xout").unwrap();
        assert_ne!(a, b);
        assert_eq!(a.as_str(), "camx/out");
        assert_eq!(b.as_str(), "cam/xout");

        // The specific collision the old fold produced: with a single `-`
        // joiner, `cam-x` + `out` and `cam` + `x-out` both rendered `cam-x-out`.
        // Under `/`-separation they are `camx.../out`-shaped and cannot collide
        // because the separator is not a chunk character.
        let hyphen_a = source_channel_name("cam-x", "out").unwrap();
        let hyphen_b = source_channel_name("cam", "x-out").unwrap();
        assert_ne!(
            hyphen_a, hyphen_b,
            "the `/`-separator must keep hyphen-bearing pairs injective"
        );
        assert_eq!(hyphen_a.as_str(), "cam-x/out");
        assert_eq!(hyphen_b.as_str(), "cam/x-out");
    }

    #[test]
    fn source_channel_name_lowercases_uppercase_leading_processor_id() {
        // Real `ProcessorUniqueId`s are `P{cuid2}` — uppercase-leading `P` over
        // a lowercase base-36 body. The derivation lowercases the processor-id
        // chunk so a valid source with a default id yields a grammar-legal
        // channel name instead of erroring. Mental-revert guard: drop the
        // `to_ascii_lowercase` normalization and this errors with
        // ChannelNameMustStartWithLowercase.
        let name = source_channel_name("Pabc123def", "video_out").unwrap();
        assert_eq!(name.as_str(), "pabc123def/video_out");
        validate_channel_name(name.as_str()).unwrap();

        // Distinct P-ids stay distinct after lowercasing (cuid2 bodies differ).
        let other = source_channel_name("Pxyz789ghi", "video_out").unwrap();
        assert_ne!(name, other);
    }

    #[test]
    fn source_channel_name_accepts_underscore_ports() {
        let name = source_channel_name("cam", "video_out").unwrap();
        assert_eq!(name.as_str(), "cam/video_out");
        validate_channel_name(name.as_str()).unwrap();
    }

    #[test]
    fn source_channel_name_rejects_out_of_grammar_port_name() {
        // A PORT name carrying a genuinely-illegal char must surface as a typed
        // error, never a silently-invalid wire name. Port names are
        // author-supplied and NOT normalized. A `/` in the port name would
        // forge an extra chunk, so it is rejected as an invalid character.
        assert_eq!(
            source_channel_name("cam", "video/in"),
            Err(IdentError::InvalidChannelNameCharacter("video/in".to_string(), '/'))
        );
        // An uppercase char in a port name is an author error — the port name
        // is not normalized, only the processor id is.
        assert_eq!(
            source_channel_name("cam", "Frame"),
            Err(IdentError::ChannelNameMustStartWithLowercase("Frame".to_string()))
        );
    }

    #[test]
    fn source_channel_name_over_bound_hashes_processor_chunk_not_port() {
        // When the joined form overflows the wire bound, only the
        // machine-generated processor chunk is shortened+hashed; the
        // author-supplied port chunk rides through whole, and the result stays
        // in bound, grammar-legal, and deterministic. Never a prefix truncation
        // across the `/`.
        let long_proc = "p".to_string() + &"processorname".repeat(6); // 79 bytes
        let a = source_channel_name(&long_proc, "output_port").unwrap();
        assert!(a.as_str().len() <= MAX_CHANNEL_NAME_BYTES);
        validate_channel_name(a.as_str()).unwrap();
        // Port chunk survives intact after the separator.
        assert!(
            a.as_str().ends_with("/output_port"),
            "author port chunk must ride through whole: {}",
            a.as_str()
        );
        // Deterministic on the hashed path.
        assert_eq!(a, source_channel_name(&long_proc, "output_port").unwrap());
        // Two long processor ids sharing a prefix must NOT collapse onto one
        // channel — the hash suffix disambiguates.
        let long_proc2 = "p".to_string() + &"processornamex".repeat(6);
        let b = source_channel_name(&long_proc2, "output_port").unwrap();
        assert_ne!(a, b, "hash suffix must disambiguate prefix-colliding processor ids");
    }
}

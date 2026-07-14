// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Drift guard for the engine↔SDK twin plugin-marshalling files.
//!
//! The engine (host-mode) and `streamlib-plugin-sdk` (cdylib-mode) carry
//! near-identical copies of the plugin marshalling logic — the engine-free SDK
//! cannot import the engine's copy, so the code is deliberately duplicated (one
//! copy binds the engine's real types in-process; the other dispatches through
//! the `#[repr(C)]` plugin-ABI vtable). The hazard is silent DRIFT: a fix that
//! lands in one copy but not the other becomes a plugin-mode-only bug the
//! host-mode tests never see.
//!
//! These tests fail `cargo test --lib` the instant the twins diverge, so the
//! divergence is caught at edit time instead of in a customer's `.slpkg`. The
//! proper fix — collapse the duplication behind one host-parameterized
//! implementation — is tracked separately; until then this guard is the safety
//! net. The whole module is `#[cfg(test)]`.

/// Engine-side twin directory, relative to this crate's manifest.
const ENGINE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/core/plugin/");
/// SDK-side twin directory (`sdk/streamlib-plugin-sdk` is two levels up).
const SDK_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../sdk/streamlib-plugin-sdk/src/plugin/"
);

/// Strip full-line comments + blank lines, apply the one known import-path shim
/// (`super::host_services::` → `super::`), then drop all remaining whitespace.
/// The result preserves the marshalling LOGIC (identifiers, calls, control
/// flow, punctuation) while normalizing away comments, import paths, and
/// line-wrap formatting — so only a real logic change makes two normalized
/// forms differ.
fn normalize(src: &str) -> String {
    let no_ws: String = src
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
        .replace("super::host_services::", "super::")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    // Trailing commas before a close-delimiter are line-wrap artifacts (rustfmt
    // adds one when it breaks an arg/field list across lines); strip them so a
    // pure wrap-formatting difference doesn't read as logic drift.
    no_ws
        .replace(",)", ")")
        .replace(",]", "]")
        .replace(",}", "}")
        .replace(",>", ">")
}

fn read(dir: &str, name: &str) -> String {
    let path = format!("{dir}{name}");
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "twin-drift guard: cannot read `{path}`: {e} — did a twin file move? update this guard."
        )
    })
}

/// FNV-1a — a deterministic (platform/version-stable) hash, unlike
/// `DefaultHasher`. Used to pin the divergent twin's content compactly.
fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// `forwarding_subscriber.rs` and `iceoryx2_log_forwarder.rs` are
/// LOGIC-IDENTICAL across the engine and the SDK (they differ only in comments
/// + the import shim). This asserts they stay that way. Unbypassable: there is
/// no fixture to update — to make this pass after a real change you MUST apply
/// the same change to both copies.
#[test]
fn logic_identical_twins_stay_in_sync() {
    for name in ["forwarding_subscriber.rs", "iceoryx2_log_forwarder.rs"] {
        let eng = normalize(&read(ENGINE_DIR, name));
        let sdk = normalize(&read(SDK_DIR, name));
        assert_eq!(
            eng, sdk,
            "\nengine↔SDK twin `{name}` has DRIFTED — a logic change landed in one \
             copy but not the other. Apply the SAME change to BOTH:\n  \
             runtime/streamlib-engine/src/core/plugin/{name}\n  \
             sdk/streamlib-plugin-sdk/src/plugin/{name}\n\
             (These two are logic-identical by contract; the engine-free SDK \
             can't reuse the engine's copy.)\n"
        );
    }
}

/// `processor_vtable.rs` LEGITIMATELY differs (engine binds real types; SDK
/// dispatches via the plugin ABI), so it can't be asserted identical. Instead
/// this is a TRIP-WIRE: any edit to either copy changes its hash, failing CI,
/// so an edit can't land silently in one host context. When it trips: verify
/// the corresponding logic in the OTHER copy, then update the expected hash
/// (the hash changing in the diff is the loud signal the divergent twin was
/// touched).
#[test]
fn divergent_processor_vtable_twin_is_tripwired() {
    // Updated whenever processor_vtable.rs is intentionally edited in either
    // copy — and updating it is the moment to confirm the matching logic landed
    // in the other copy too.
    const EXPECTED_ENGINE: u64 = 0xd35f_5cb9_a23a_b197;
    const EXPECTED_SDK: u64 = 0xdbcd_b43d_7cbc_8b5e;
    let eng = fnv1a(&normalize(&read(ENGINE_DIR, "processor_vtable.rs")));
    let sdk = fnv1a(&normalize(&read(SDK_DIR, "processor_vtable.rs")));
    assert!(
        eng == EXPECTED_ENGINE && sdk == EXPECTED_SDK,
        "\nprocessor_vtable.rs twin trip-wire fired — a copy was edited.\n\
         Verify the same logic change belongs in the OTHER copy \
         (runtime/streamlib-engine/src/core/plugin/ AND \
         sdk/streamlib-plugin-sdk/src/plugin/), then set:\n  \
         EXPECTED_ENGINE = {eng:#018x}\n  EXPECTED_SDK = {sdk:#018x}\n"
    );
}

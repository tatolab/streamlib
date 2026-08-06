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

/// Engine-side `EmptyConfig` serde twin file (outside the `plugin/` dir).
const ENGINE_EMPTY_CONFIG_FILE: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/src/core/processors/mod.rs");
/// SDK-side `EmptyConfig` serde twin file (outside the `plugin/` dir).
const SDK_EMPTY_CONFIG_FILE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../sdk/streamlib-plugin-sdk/src/processors.rs"
);
/// Line marker bracketing the guarded `EmptyConfig` serde region in each twin.
const EMPTY_CONFIG_TWIN_BEGIN: &str = "twin-guard(empty-config-serde): BEGIN";
const EMPTY_CONFIG_TWIN_END: &str = "twin-guard(empty-config-serde): END";

/// Engine-side runtime-shutdown control-topic publish twin.
const ENGINE_RUNTIME_SHUTDOWN_PUBLISH_FILE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/core/runtime/runtime_shutdown_request.rs"
);
/// SDK-side runtime-shutdown control-topic publish twin.
const SDK_RUNTIME_SHUTDOWN_PUBLISH_FILE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../sdk/streamlib-plugin-sdk/src/runtime_control.rs"
);
/// Line marker bracketing the guarded publish helper in each twin.
const RUNTIME_SHUTDOWN_PUBLISH_TWIN_BEGIN: &str = "twin-guard(runtime-shutdown-publish): BEGIN";
const RUNTIME_SHUTDOWN_PUBLISH_TWIN_END: &str = "twin-guard(runtime-shutdown-publish): END";

/// Engine-side `HostServices` → `HostCallbacks` field-map twin.
const ENGINE_HOST_CALLBACKS_FIELD_MAP_FILE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/core/plugin/host_services/mod.rs"
);
/// SDK-side `HostServices` → `HostCallbacks` field-map twin.
const SDK_HOST_CALLBACKS_FIELD_MAP_FILE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../sdk/streamlib-plugin-sdk/src/plugin.rs"
);
/// Line marker bracketing the guarded field map in each twin.
const HOST_CALLBACKS_FIELD_MAP_TWIN_BEGIN: &str = "twin-guard(host-callbacks-field-map): BEGIN";
const HOST_CALLBACKS_FIELD_MAP_TWIN_END: &str = "twin-guard(host-callbacks-field-map): END";

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

fn read_path(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| {
        panic!(
            "twin-drift guard: cannot read `{path}`: {e} — did a twin file move? update this guard."
        )
    })
}

/// Extract the lines strictly between the `BEGIN`/`END` marker lines. Panics if
/// either marker is missing (a twin file whose guard region was renamed/removed
/// must not silently pass this guard).
fn extract_marked_region(src: &str, path: &str, begin_marker: &str, end_marker: &str) -> String {
    let lines: Vec<&str> = src.lines().collect();
    let begin = lines
        .iter()
        .position(|l| l.contains(begin_marker))
        .unwrap_or_else(|| panic!("twin-drift guard: `{begin_marker}` marker missing in `{path}`"));
    let end = lines
        .iter()
        .position(|l| l.contains(end_marker))
        .unwrap_or_else(|| panic!("twin-drift guard: `{end_marker}` marker missing in `{path}`"));
    lines[begin + 1..end].join("\n")
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

/// The `install_host_services` version-skew diagnostic (M32 #1253) is factored
/// into a `layout_skew_diagnostic.rs` file in each twin so the two are
/// LOGIC-IDENTICAL — the outer + per-inner-vtable `layout_version` checks and
/// the `report_layout_skew` emitter are the same in both host contexts. The
/// two files sit at different relative sub-paths (engine
/// `host_services/layout_skew_diagnostic.rs`, SDK
/// `layout_skew_diagnostic.rs`), so this is a path-pair entry, not the
/// same-filename loop above. Unbypassable: to make it pass after a real change
/// you MUST apply the same change to BOTH — the skew diagnostic is a
/// plugin-mode-only refusal path host-mode tests never exercise.
#[test]
fn logic_identical_install_skew_diagnostic_twins_stay_in_sync() {
    let eng = normalize(&read(ENGINE_DIR, "host_services/layout_skew_diagnostic.rs"));
    let sdk = normalize(&read(SDK_DIR, "layout_skew_diagnostic.rs"));
    assert_eq!(
        eng, sdk,
        "\nengine↔SDK `install_host_services` skew-diagnostic twin has DRIFTED — \
         a logic change landed in one copy but not the other. Apply the SAME \
         change to BOTH:\n  \
         runtime/streamlib-engine/src/core/plugin/host_services/layout_skew_diagnostic.rs\n  \
         sdk/streamlib-plugin-sdk/src/plugin/layout_skew_diagnostic.rs\n\
         (These two are logic-identical by contract; the engine-free SDK \
         can't reuse the engine's copy, so a version-skew fix must land in both \
         install_host_services twins.)\n"
    );
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
    const EXPECTED_ENGINE: u64 = 0xbee1_89ef_1526_2cef;
    const EXPECTED_SDK: u64 = 0xcf16_0d89_418b_8674;
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

/// The runtime-shutdown request's cdylib-side publish is a wire-load-bearing
/// twin: the engine's copy runs in a facade cdylib (which statically links the
/// engine) and the SDK's in an engine-free one, and both must hand the host the
/// SAME `(reserved control topic, msgpack reason)` pair. The host decodes the
/// reason with `unwrap_or_default()` and shuts down anyway, so a one-sided edit
/// is a silent attribution loss, never a failure. Only the publish helper is
/// twinned in each file, so this is a marked-region entry. Unbypassable: there
/// is no fixture to update — apply the same change to BOTH.
#[test]
fn logic_identical_runtime_shutdown_publish_twins_stay_in_sync() {
    let eng = normalize(&extract_marked_region(
        &read_path(ENGINE_RUNTIME_SHUTDOWN_PUBLISH_FILE),
        ENGINE_RUNTIME_SHUTDOWN_PUBLISH_FILE,
        RUNTIME_SHUTDOWN_PUBLISH_TWIN_BEGIN,
        RUNTIME_SHUTDOWN_PUBLISH_TWIN_END,
    ));
    let sdk = normalize(&extract_marked_region(
        &read_path(SDK_RUNTIME_SHUTDOWN_PUBLISH_FILE),
        SDK_RUNTIME_SHUTDOWN_PUBLISH_FILE,
        RUNTIME_SHUTDOWN_PUBLISH_TWIN_BEGIN,
        RUNTIME_SHUTDOWN_PUBLISH_TWIN_END,
    ));
    assert_eq!(
        eng, sdk,
        "\nengine↔SDK runtime-shutdown publish twin has DRIFTED — a wire change \
         landed in one copy but not the other. Apply the SAME change to BOTH:\n  \
         runtime/streamlib-engine/src/core/runtime/runtime_shutdown_request.rs\n  \
         sdk/streamlib-plugin-sdk/src/runtime_control.rs\n\
         (Both publish the reserved runtime-shutdown control topic; the \
         engine-free SDK can't reuse the engine's copy.)\n"
    );
}

/// The `HostServices` → `HostCallbacks` field map is the load handshake's last
/// step, and it exists twice: the engine's copy runs in a facade cdylib, the
/// SDK's in an engine-free one. A slot added to only one copy reaches only one
/// cdylib flavor — a silent, flavor-dependent null callback, never a build
/// failure. Only the mapping function is twinned in each file, so this is a
/// marked-region entry. Unbypassable: there is no fixture to update — apply the
/// same change to BOTH.
#[test]
fn logic_identical_host_callbacks_field_map_twins_stay_in_sync() {
    let eng = normalize(&extract_marked_region(
        &read_path(ENGINE_HOST_CALLBACKS_FIELD_MAP_FILE),
        ENGINE_HOST_CALLBACKS_FIELD_MAP_FILE,
        HOST_CALLBACKS_FIELD_MAP_TWIN_BEGIN,
        HOST_CALLBACKS_FIELD_MAP_TWIN_END,
    ));
    let sdk = normalize(&extract_marked_region(
        &read_path(SDK_HOST_CALLBACKS_FIELD_MAP_FILE),
        SDK_HOST_CALLBACKS_FIELD_MAP_FILE,
        HOST_CALLBACKS_FIELD_MAP_TWIN_BEGIN,
        HOST_CALLBACKS_FIELD_MAP_TWIN_END,
    ));
    assert_eq!(
        eng, sdk,
        "\nengine↔SDK HostCallbacks field-map twin has DRIFTED — a slot landed \
         in one copy but not the other, so it reaches only one cdylib flavor. \
         Apply the SAME change to BOTH:\n  \
         runtime/streamlib-engine/src/core/plugin/host_services/mod.rs\n  \
         sdk/streamlib-plugin-sdk/src/plugin.rs\n"
    );
}

/// The marked-region guards above compare NORMALIZED text, so their value rests
/// on [`normalize`] not erasing the load-bearing tokens. This drives the
/// comparison against deliberately drifted copies of the runtime-shutdown
/// publish region: a swapped msgpack encoder or a swapped topic constant must
/// read as drift, while a comment-only edit must not. Without this, a
/// `normalize` that over-collapsed would leave every marked-region guard
/// silently vacuous.
#[test]
fn the_marked_region_comparison_detects_a_one_sided_wire_change() {
    let region = extract_marked_region(
        &read_path(ENGINE_RUNTIME_SHUTDOWN_PUBLISH_FILE),
        ENGINE_RUNTIME_SHUTDOWN_PUBLISH_FILE,
        RUNTIME_SHUTDOWN_PUBLISH_TWIN_BEGIN,
        RUNTIME_SHUTDOWN_PUBLISH_TWIN_END,
    );
    let baseline = normalize(&region);

    for (what_drifted, drifted) in [
        (
            "the msgpack encoder",
            region.replace("rmp_serde::to_vec(", "rmp_serde::to_vec_named("),
        ),
        (
            "the reserved control topic",
            region.replace(
                "PUBSUB_CONTROL_TOPIC_RUNTIME_SHUTDOWN_REQUEST",
                "PUBSUB_CONTROL_TOPIC_SOME_OTHER_REQUEST",
            ),
        ),
    ] {
        assert_ne!(
            drifted, region,
            "the {what_drifted} substitution matched nothing — this test no \
             longer exercises what it claims"
        );
        assert_ne!(
            baseline,
            normalize(&drifted),
            "normalize() erased {what_drifted}, so the marked-region twin \
             guards would not catch a one-sided change to it"
        );
    }

    assert_eq!(
        baseline,
        normalize(&format!("// a comment-only edit\n{region}")),
        "normalize() must ignore comment-only edits, or every marked-region \
         guard fails on prose churn"
    );
}

/// `EmptyConfig`'s `Serialize`/`Deserialize` is a wire-load-bearing twin: config
/// crosses the plugin ABI, so the host's copy
/// (`core/processors/mod.rs`) and the engine-free SDK's copy
/// (`streamlib-plugin-sdk/src/processors.rs`) MUST serialize to the same empty
/// named map and tolerate the same decode shapes. The two copies LEGITIMATELY
/// differ in path-qualification (`serde::` prefix) and serialize style (engine
/// UFCS vs SDK method-chain), so they can't be asserted identical — this is a
/// TRIP-WIRE like `divergent_processor_vtable_twin_is_tripwired`: any edit to
/// either `EmptyConfig` serde region changes its hash, failing CI, so a one-sided
/// edit can't ship in a `.slpkg`. When it trips: confirm the matching wire-shape
/// change landed in the OTHER copy, then update the expected hash.
#[test]
fn divergent_empty_config_serde_twin_is_tripwired() {
    // Updated whenever an EmptyConfig serde region is intentionally edited in
    // either copy — updating it is the moment to confirm the wire shape stayed
    // identical across the ABI.
    const EXPECTED_ENGINE: u64 = 0x6a69_bb57_54ed_0c2d;
    const EXPECTED_SDK: u64 = 0xc993_bbca_439a_6d75;
    let eng = fnv1a(&normalize(&extract_marked_region(
        &read_path(ENGINE_EMPTY_CONFIG_FILE),
        ENGINE_EMPTY_CONFIG_FILE,
        EMPTY_CONFIG_TWIN_BEGIN,
        EMPTY_CONFIG_TWIN_END,
    )));
    let sdk = fnv1a(&normalize(&extract_marked_region(
        &read_path(SDK_EMPTY_CONFIG_FILE),
        SDK_EMPTY_CONFIG_FILE,
        EMPTY_CONFIG_TWIN_BEGIN,
        EMPTY_CONFIG_TWIN_END,
    )));
    assert!(
        eng == EXPECTED_ENGINE && sdk == EXPECTED_SDK,
        "\nEmptyConfig serde twin trip-wire fired — a copy was edited.\n\
         Verify the same wire-shape change belongs in the OTHER copy \
         (runtime/streamlib-engine/src/core/processors/mod.rs AND \
         sdk/streamlib-plugin-sdk/src/processors.rs), then set:\n  \
         EXPECTED_ENGINE = {eng:#018x}\n  EXPECTED_SDK = {sdk:#018x}\n"
    );
}

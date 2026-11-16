// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use tempfile::TempDir;

fn write_fixture(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, content).unwrap();
}

fn empty_workspace() -> TempDir {
    let dir = TempDir::new().unwrap();
    // Top-level Cargo.toml so walk_cargo_toml has something to find at
    // root if we choose to add deps there.
    write_fixture(dir.path(), "Cargo.toml", "[workspace]\nmembers = []\n");
    dir
}

// ----- no ash -----

#[test]
fn rejects_ash_in_cargo_toml_runtime_dep() {
    let dir = empty_workspace();
    write_fixture(
        dir.path(),
        "runtime/streamlib-engine/Cargo.toml",
        "[package]\nname = \"streamlib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nash = \"0.38\"\n",
    );
    let report = scan_all(dir.path()).unwrap();
    assert!(
        report.violations.iter().any(|v| v.check == CHECK_NO_ASH),
        "expected ash Cargo dep violation, got {:?}",
        report.violations,
    );
}

#[test]
fn ignores_ahash_lookalike() {
    let dir = empty_workspace();
    // Cargo.toml with ahash dep.
    write_fixture(
        dir.path(),
        "runtime/streamlib-engine/Cargo.toml",
        "[package]\nname = \"streamlib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nahash = \"0.8\"\n",
    );
    let report = scan_all(dir.path()).unwrap();
    let no_ash: Vec<_> = report
        .violations
        .iter()
        .filter(|v| v.check == CHECK_NO_ASH)
        .collect();
    assert!(
        no_ash.is_empty(),
        "ahash should not match ash: {:?}",
        no_ash
    );
}

// ----- vulkanalia confined -----

#[test]
fn rejects_vulkanalia_cargo_dep_outside_allowlist() {
    let dir = empty_workspace();
    write_fixture(
        dir.path(),
        "sdk/streamlib-error/Cargo.toml",
        r#"[package]
name = "streamlib-error"
version = "0.1.0"
edition = "2021"

[dependencies]
vulkanalia = "0.20"
"#,
    );
    let report = scan_all(dir.path()).unwrap();
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.check == CHECK_VULKANALIA),
        "expected vulkanalia Cargo dep violation, got {:?}",
        report.violations,
    );
}

#[test]
fn allows_vulkanalia_cargo_dep_in_adapter_crate() {
    let dir = empty_workspace();
    write_fixture(
        dir.path(),
        "adapters/streamlib-adapter-vulkan/Cargo.toml",
        r#"[package]
name = "streamlib-adapter-vulkan"
version = "0.1.0"
edition = "2021"

[dependencies]
vulkanalia = "0.20"
"#,
    );
    let report = scan_all(dir.path()).unwrap();
    let vk: Vec<_> = report
        .violations
        .iter()
        .filter(|v| v.check == CHECK_VULKANALIA)
        .collect();
    assert!(
        vk.is_empty(),
        "vulkanalia dep in adapter crate should pass: {:?}",
        vk
    );
}

// ----- vulkanalia uses workspace fork -----

#[test]
fn rejects_direct_vulkanalia_version_in_member() {
    let dir = empty_workspace();
    write_fixture(
        dir.path(),
        "adapters/streamlib-adapter-vulkan/Cargo.toml",
        r#"[package]
name = "streamlib-adapter-vulkan"
version = "0.1.0"
edition = "2021"

[dependencies]
vulkanalia = "0.35"
"#,
    );
    let report = scan_all(dir.path()).unwrap();
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.check == CHECK_VULKANALIA_FORK),
        "expected workspace-fork violation, got {:?}",
        report.violations,
    );
}

#[test]
fn rejects_direct_vulkanalia_vma_version_in_member() {
    let dir = empty_workspace();
    write_fixture(
        dir.path(),
        "runtime/test-member/Cargo.toml",
        r#"[package]
name = "test-member"
version = "0.1.0"
edition = "2021"

[dependencies]
vulkanalia-vma = "0.4"
"#,
    );
    let report = scan_all(dir.path()).unwrap();
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.check == CHECK_VULKANALIA_FORK),
        "expected workspace-fork violation for vulkanalia-vma, got {:?}",
        report.violations,
    );
}

#[test]
fn rejects_direct_vulkanalia_git_in_member() {
    let dir = empty_workspace();
    // Even a git URL in a member crate is a violation: it bypasses the
    // single source of truth at workspace level.
    write_fixture(
        dir.path(),
        "adapters/streamlib-adapter-opengl/Cargo.toml",
        r#"[package]
name = "streamlib-adapter-opengl"
version = "0.1.0"
edition = "2021"

[dependencies]
vulkanalia = { git = "https://github.com/KhronosGroup/Vulkan-Headers" }
"#,
    );
    let report = scan_all(dir.path()).unwrap();
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.check == CHECK_VULKANALIA_FORK),
        "expected workspace-fork violation for git dep, got {:?}",
        report.violations,
    );
}

#[test]
fn rejects_direct_tatolab_vulkanalia_dep_in_member() {
    let dir = empty_workspace();
    // A direct dep on the vendored crate NAME bypasses the workspace
    // rename exactly like a direct `vulkanalia = "0.35"` bypassed the
    // fork — `check_vulkanalia_uses_workspace_fork` must flag it.
    write_fixture(
        dir.path(),
        "adapters/streamlib-adapter-vulkan/Cargo.toml",
        r#"[package]
name = "streamlib-adapter-vulkan"
version = "0.1.0"
edition = "2021"

[dependencies]
tatolab-vulkanalia = "0.35"
"#,
    );
    let report = scan_all(dir.path()).unwrap();
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.check == CHECK_VULKANALIA_FORK),
        "expected workspace-fork violation for direct tatolab-vulkanalia dep, got {:?}",
        report.violations,
    );
}

#[test]
fn allows_vendored_fork_sibling_deps_and_sources() {
    let dir = empty_workspace();
    // The vendored fork crates' own sibling deps cannot be
    // workspace-inherited, and their sources ARE the Vulkan surface —
    // both `check_vulkanalia_confined` and `check_vulkanalia_uses_workspace_fork` exempt vendor/tatolab-vulkanalia*.
    write_fixture(
        dir.path(),
        "vendor/tatolab-vulkanalia-vma/Cargo.toml",
        r#"[package]
name = "tatolab-vulkanalia-vma"
version = "0.9.0"
edition = "2021"

[dependencies]
vulkanalia = { package = "tatolab-vulkanalia", version = "0.35", path = "../tatolab-vulkanalia", default-features = false }
"#,
    );
    let report = scan_all(dir.path()).unwrap();
    let hits: Vec<_> = report
        .violations
        .iter()
        .filter(|v| v.check == CHECK_VULKANALIA || v.check == CHECK_VULKANALIA_FORK)
        .collect();
    assert!(
        hits.is_empty(),
        "vendored fork dirs should be exempt from `check_vulkanalia_confined` and `check_vulkanalia_uses_workspace_fork`: {:?}",
        hits
    );
}

#[test]
fn vendored_dir_lookalike_does_not_inherit_exemptions() {
    let dir = empty_workspace();
    // The vendored-dir exemptions are anchored to the three EXACT dirs;
    // a hypothetical vendor/tatolab-vulkanalia-extras/ crate must NOT
    // silently inherit them in either `check_vulkanalia_confined` or `check_vulkanalia_uses_workspace_fork`.
    write_fixture(
        dir.path(),
        "vendor/tatolab-vulkanalia-extras/Cargo.toml",
        r#"[package]
name = "tatolab-vulkanalia-extras"
version = "0.1.0"
edition = "2021"

[dependencies]
vulkanalia = { package = "tatolab-vulkanalia", version = "0.35", path = "../tatolab-vulkanalia" }
"#,
    );
    let report = scan_all(dir.path()).unwrap();
    assert!(
        report.violations.iter().any(|v| v.check == CHECK_VULKANALIA
            && v.path
                .to_string_lossy()
                .contains("tatolab-vulkanalia-extras")),
        "lookalike dir must trip `check_vulkanalia_confined`, got {:?}",
        report.violations,
    );
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.check == CHECK_VULKANALIA_FORK
                && v.path
                    .to_string_lossy()
                    .contains("tatolab-vulkanalia-extras")),
        "lookalike dir must trip `check_vulkanalia_uses_workspace_fork`, got {:?}",
        report.violations,
    );
}

#[test]
fn rejects_package_rename_bypass_in_member() {
    let dir = empty_workspace();
    // `foo = { package = "tatolab-vulkanalia", … }` grants the same raw
    // Vulkan surface under an arbitrary dep key — `check_vulkanalia_uses_workspace_fork` must match the
    // value's `package` field, not just the dep-table key.
    write_fixture(
        dir.path(),
        "adapters/streamlib-adapter-vulkan/Cargo.toml",
        r#"[package]
name = "streamlib-adapter-vulkan"
version = "0.1.0"
edition = "2021"

[dependencies]
foo = { package = "tatolab-vulkanalia", path = "../tatolab-vulkanalia", version = "0.35" }
"#,
    );
    let report = scan_all(dir.path()).unwrap();
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.check == CHECK_VULKANALIA_FORK
                && v.matched_pattern
                    .contains("package = \"tatolab-vulkanalia\"")),
        "expected workspace-fork violation for the package= rename bypass, got {:?}",
        report.violations,
    );
}

#[test]
fn rejects_direct_tatolab_vulkanalia_dep_outside_rhi_confinement() {
    let dir = empty_workspace();
    // `check_vulkanalia_confined`'s Cargo dep scan must also catch the vendored crate name —
    // a non-RHI crate gaining raw Vulkan via `tatolab-vulkanalia*`.
    write_fixture(
        dir.path(),
        "sdk/streamlib-error/Cargo.toml",
        r#"[package]
name = "streamlib-error"
version = "0.1.0"
edition = "2021"

[dependencies]
tatolab-vulkanalia-vma = "0.9"
"#,
    );
    let report = scan_all(dir.path()).unwrap();
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.check == CHECK_VULKANALIA),
        "expected vulkanalia confinement violation for direct tatolab-vulkanalia-vma dep, got {:?}",
        report.violations,
    );
}

#[test]
fn allows_workspace_inline_vulkanalia_dep() {
    let dir = empty_workspace();
    write_fixture(
        dir.path(),
        "adapters/streamlib-adapter-vulkan/Cargo.toml",
        r#"[package]
name = "streamlib-adapter-vulkan"
version = "0.1.0"
edition = "2021"

[dependencies]
vulkanalia = { workspace = true }
"#,
    );
    let report = scan_all(dir.path()).unwrap();
    let fork: Vec<_> = report
        .violations
        .iter()
        .filter(|v| v.check == CHECK_VULKANALIA_FORK)
        .collect();
    assert!(
        fork.is_empty(),
        "{{ workspace = true }} should pass: {:?}",
        fork
    );
}

#[test]
fn allows_workspace_dotted_vulkanalia_dep() {
    let dir = empty_workspace();
    write_fixture(
        dir.path(),
        "adapters/streamlib-adapter-vulkan/Cargo.toml",
        r#"[package]
name = "streamlib-adapter-vulkan"
version = "0.1.0"
edition = "2021"

[dependencies]
vulkanalia.workspace = true
"#,
    );
    let report = scan_all(dir.path()).unwrap();
    let fork: Vec<_> = report
        .violations
        .iter()
        .filter(|v| v.check == CHECK_VULKANALIA_FORK)
        .collect();
    assert!(fork.is_empty(), "dotted-key form should pass: {:?}", fork);
}

// ----- cdylib + adapter runtime deps -----

#[test]
fn rejects_streamlib_in_cdylib_runtime_dep() {
    let dir = empty_workspace();
    write_fixture(
        dir.path(),
        "adapters/streamlib-adapter-vulkan/Cargo.toml",
        r#"[package]
name = "streamlib-adapter-vulkan"
version = "0.1.0"
edition = "2021"

[dependencies]
streamlib = { path = "../streamlib" }
"#,
    );
    let report = scan_all(dir.path()).unwrap();
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.check == CHECK_CDYLIB_DEPS),
        "expected cdylib runtime-dep violation, got {:?}",
        report.violations,
    );
}

#[test]
fn rejects_streamlib_in_adapter_runtime_dep() {
    let dir = empty_workspace();
    write_fixture(
        dir.path(),
        "adapters/streamlib-adapter-vulkan/Cargo.toml",
        r#"[package]
name = "streamlib-adapter-vulkan"
version = "0.1.0"
edition = "2021"

[target.'cfg(target_os = "linux")'.dependencies]
streamlib = { path = "../streamlib" }
"#,
    );
    let report = scan_all(dir.path()).unwrap();
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.check == CHECK_CDYLIB_DEPS),
        "expected adapter target runtime-dep violation, got {:?}",
        report.violations,
    );
}

#[test]
fn allows_streamlib_in_cdylib_dev_dep() {
    let dir = empty_workspace();
    write_fixture(
        dir.path(),
        "adapters/streamlib-adapter-cpu-readback/Cargo.toml",
        r#"[package]
name = "streamlib-adapter-cpu-readback"
version = "0.1.0"
edition = "2021"

[target.'cfg(target_os = "linux")'.dev-dependencies]
streamlib = { path = "../streamlib" }
"#,
    );
    let report = scan_all(dir.path()).unwrap();
    let cdy: Vec<_> = report
        .violations
        .iter()
        .filter(|v| v.check == CHECK_CDYLIB_DEPS)
        .collect();
    assert!(
        cdy.is_empty(),
        "streamlib in dev-deps should pass: {:?}",
        cdy
    );
}

#[test]
fn rejects_vulkanalia_cargo_dep_in_the_wheel() {
    let dir = empty_workspace();
    write_fixture(
        dir.path(),
        "sdk/streamlib-python-wheel/Cargo.toml",
        r#"[package]
name = "streamlib-python-wheel"
version = "0.1.0"
edition = "2021"

[target.'cfg(target_os = "linux")'.dependencies]
vulkanalia = { workspace = true }
"#,
    );
    let report = scan_all(dir.path()).unwrap();
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.check == CHECK_VULKANALIA),
        "expected vulkanalia Cargo-dep violation in the wheel, got {:?}",
        report.violations,
    );
}

// ----- packages/* facade-dep ban -----

#[test]
fn rejects_facade_dep_in_new_package() {
    let dir = empty_workspace();
    // A newly-carved package that is NOT on the seeded baseline must trip
    // the ratchet the moment it links the full facade.
    write_fixture(
        dir.path(),
        "packages/newly-carved/Cargo.toml",
        r#"[package]
name = "streamlib-newly-carved"
version = "0.1.0"
edition = "2021"

[dependencies]
streamlib = { version = "0.6.0" }
"#,
    );
    let report = scan_all(dir.path()).unwrap();
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.check == CHECK_PACKAGES_FACADE_DEP),
        "expected packages facade-dep violation, got {:?}",
        report.violations,
    );
}

#[test]
fn allows_facade_dep_in_allowlisted_package() {
    let dir = empty_workspace();
    // packages/test-fixtures is a permanent host-by-design linker — its
    // facade dep must NOT trip the ratchet.
    write_fixture(
        dir.path(),
        "packages/test-fixtures/Cargo.toml",
        r#"[package]
name = "streamlib-test-fixtures"
version = "0.1.0"
edition = "2021"

[dependencies]
streamlib = { version = "0.6.0" }
"#,
    );
    let report = scan_all(dir.path()).unwrap();
    let hits: Vec<_> = report
        .violations
        .iter()
        .filter(|v| v.check == CHECK_PACKAGES_FACADE_DEP)
        .collect();
    assert!(
        hits.is_empty(),
        "allowlisted package facade dep should pass: {:?}",
        hits
    );
}

#[test]
fn allows_facade_dev_dep_in_new_package() {
    let dir = empty_workspace();
    // The escape hatch: move the facade to [dev-dependencies] and a
    // non-allowlisted package passes (dev-only is exempt, per `check_cdylib_and_adapter_runtime_deps`).
    write_fixture(
        dir.path(),
        "packages/newly-carved/Cargo.toml",
        r#"[package]
name = "streamlib-newly-carved"
version = "0.1.0"
edition = "2021"

[dev-dependencies]
streamlib = { version = "0.6.0" }
"#,
    );
    let report = scan_all(dir.path()).unwrap();
    let hits: Vec<_> = report
        .violations
        .iter()
        .filter(|v| v.check == CHECK_PACKAGES_FACADE_DEP)
        .collect();
    assert!(
        hits.is_empty(),
        "facade dep in dev-dependencies should pass: {:?}",
        hits
    );
}

// ----- trunk-set -> streamlib-engine Cargo-dep ban -----

#[test]
fn rejects_direct_engine_dep_in_trunk_crate() {
    let dir = empty_workspace();
    // A trunk crate that adds a DIRECT [dependencies] streamlib-engine dep
    // must trip the permanent ban — streamlib-macros is consumed by version
    // from the registry, so this would propagate the engine to every
    // external consumer invisibly.
    write_fixture(
        dir.path(),
        "sdk/streamlib-macros/Cargo.toml",
        r#"[package]
name = "streamlib-macros"
version = "0.1.0"
edition = "2021"

[dependencies]
streamlib-engine = { path = "../../runtime/streamlib-engine" }
"#,
    );
    let report = scan_all(dir.path()).unwrap();
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.check == CHECK_TRUNK_NO_ENGINE_DEP),
        "expected trunk-set engine-dep violation, got {:?}",
        report.violations,
    );
}

#[test]
fn rejects_aliased_engine_dep_in_trunk_crate() {
    let dir = empty_workspace();
    // An ALIASED dep key whose `package =` resolves to streamlib-engine
    // must be caught by its RESOLVED package name, not the section key —
    // locks the alias resolution that closes the facade-check evasion.
    write_fixture(
        dir.path(),
        "sdk/streamlib-macros/Cargo.toml",
        r#"[package]
name = "streamlib-macros"
version = "0.1.0"
edition = "2021"

[dependencies]
x = { package = "streamlib-engine", path = "../../runtime/streamlib-engine" }
"#,
    );
    let report = scan_all(dir.path()).unwrap();
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.check == CHECK_TRUNK_NO_ENGINE_DEP
                && v.matched_pattern.contains("package = \"streamlib-engine\"")),
        "expected trunk-set aliased engine-dep violation, got {:?}",
        report.violations,
    );
}

#[test]
fn rejects_direct_engine_dep_in_consumer_rhi_trunk_crate() {
    let dir = empty_workspace();
    // consumer-rhi is a trunk root (#1252 follow-up): adapter
    // crates dep it directly and consume it by version, so a
    // non-dev streamlib-engine dep in it would propagate the FullAccess
    // engine surface to every external consumer invisibly. Locks
    // consumer-rhi's root membership specifically — distinct from the
    // macros case above.
    write_fixture(
        dir.path(),
        "runtime/streamlib-consumer-rhi/Cargo.toml",
        r#"[package]
name = "streamlib-consumer-rhi"
version = "0.1.0"
edition = "2021"

[dependencies]
streamlib-engine = { path = "../streamlib-engine" }
"#,
    );
    let report = scan_all(dir.path()).unwrap();
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.check == CHECK_TRUNK_NO_ENGINE_DEP),
        "expected consumer-rhi trunk-set engine-dep violation, got {:?}",
        report.violations,
    );
}

#[test]
fn allows_engine_dev_dep_in_trunk_crate() {
    let dir = empty_workspace();
    // [dev-dependencies] are exempt — a trunk crate's conformance tests may
    // legitimately pull the engine to exercise the host backing.
    write_fixture(
        dir.path(),
        "sdk/streamlib-macros/Cargo.toml",
        r#"[package]
name = "streamlib-macros"
version = "0.1.0"
edition = "2021"

[dev-dependencies]
streamlib-engine = { path = "../../runtime/streamlib-engine" }
"#,
    );
    let report = scan_all(dir.path()).unwrap();
    let hits: Vec<_> = report
        .violations
        .iter()
        .filter(|v| v.check == CHECK_TRUNK_NO_ENGINE_DEP)
        .collect();
    assert!(
        hits.is_empty(),
        "engine dep in trunk [dev-dependencies] should pass: {:?}",
        hits
    );
}

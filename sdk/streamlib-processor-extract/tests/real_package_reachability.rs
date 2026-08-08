// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Reachability resolution over REAL in-tree packages.
//!
//! `@tatolab/camera` is the canonical over-collection case: its Linux arm
//! (`processors/camera_linux.rs`) and its parked Apple arm
//! (`processors/_apple_impl_pending_/camera.rs`) BOTH declare a
//! `@tatolab/camera/Camera` processor. The parked arm carries a file-level
//! `#![cfg(any())]`, so it never compiles on any target. This locks that:
//!
//! - the raw whole-tree scan over-collects (two `Camera`s), and
//! - the reachability-resolved scan yields exactly the set the package's
//!   committed `processors:` manifest lists, per target — no parked duplicate.
//!
//! Every assertion here is hard. A layout-coupled test that returns early when
//! the layout it couples to is absent goes green the instant the source moves,
//! which is exactly when it should be loudest.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use streamlib_processor_extract::crate_root::{
    RustCrateRootGenerationRequest, discover_package_dirs_declaring_a_generated_crate_root,
    generate_rust_crate_root_source,
};
use streamlib_processor_extract::{
    ModuleReachabilityTarget, ProcessorAvailabilityAcrossBuildTargets,
    extract_processors_across_every_build_target, extract_reachable_rust_processors,
    extract_rust_processors,
};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn package_dir(name: &str) -> PathBuf {
    crate_dir_under("packages", name)
}

/// A crate that authors processors but does not live under `packages/`. The
/// api-server is the case: it is engine-side host infrastructure in `runtime/`,
/// not a distributable package, and `processors/` is still its discovery root.
fn engine_crate_dir(name: &str) -> PathBuf {
    crate_dir_under("runtime", name)
}

fn crate_dir_under(tree: &str, name: &str) -> PathBuf {
    let dir = workspace_root().join(tree).join(name);
    assert!(
        dir.join("processors").is_dir(),
        "{tree}/{name} must author its processor modules under `processors/` — \
         folder-backed discovery scans no other root, so a moved crate would \
         derive the empty set"
    );
    dir
}

fn target_for(os: &str) -> ModuleReachabilityTarget {
    ModuleReachabilityTarget::new()
        .with_key_value("target_os", os)
        .with_key_value("target_arch", "x86_64")
        .with_key_value(
            "target_family",
            if os == "windows" { "windows" } else { "unix" },
        )
        .with_flag(if os == "windows" { "windows" } else { "unix" })
}

fn sorted_names(procs: Vec<streamlib_processor_extract::ExtractedProcessor>) -> Vec<String> {
    let mut names: Vec<String> = procs.into_iter().map(|p| p.schema.name).collect();
    names.sort();
    names
}

fn reachable_names(dir: &Path, os: &str) -> Vec<String> {
    sorted_names(extract_reachable_rust_processors(dir, &target_for(os)).unwrap())
}

/// The availability entry a package's across-every-target scan resolves for one
/// processor `Type`.
fn availability_of(
    dir: &Path,
    processor_type_name: &str,
) -> ProcessorAvailabilityAcrossBuildTargets {
    extract_processors_across_every_build_target(dir)
        .unwrap_or_else(|e| panic!("scanning {}: {e}", dir.display()))
        .availability_of_processor_type_name(processor_type_name)
        .unwrap_or_else(|| {
            panic!(
                "{} must declare `{processor_type_name}` on some target",
                dir.display()
            )
        })
        .clone()
}

/// The build targets, out of the ones the in-tree packages actually split on,
/// a processor is available for.
fn available_build_target_operating_systems(
    availability: &ProcessorAvailabilityAcrossBuildTargets,
) -> Vec<String> {
    ["linux", "macos", "ios", "windows"]
        .into_iter()
        .filter(|os| availability.is_available_on_build_target(&target_for(os)))
        .map(str::to_string)
        .collect()
}

#[test]
fn raw_scan_over_collects_the_parked_apple_arm() {
    let dir = package_dir("camera");
    let names = sorted_names(extract_rust_processors(&dir).unwrap());
    // Both the Linux and the parked Apple arm declare `Camera` → duplicate.
    let camera_count = names.iter().filter(|n| n.as_str() == "Camera").count();
    assert!(
        camera_count >= 2,
        "raw scan should over-collect the parked Apple `Camera`; got {names:?}"
    );
}

#[test]
fn camera_reachable_scan_matches_the_committed_manifest_on_linux() {
    let names = reachable_names(&package_dir("camera"), "linux");
    assert_eq!(
        names,
        vec!["Camera".to_string(), "CameraToCudaCopy".to_string()],
        "reachable Linux scan must equal the package's `processors:` set, \
         with the parked Apple `Camera` excluded"
    );
}

/// Behavior delta pinned: `@tatolab/camera` on macOS/iOS used to emit no
/// `STREAMLIB_PLUGIN` at all (its whole `export_plugin!` was
/// `#[cfg(target_os = "linux")]`), so the loader rejected it with a
/// missing-symbol error. The unconditional `CameraToCudaCopy` arm now carries
/// the declaration, so the package loads and the processor fails in `setup()`
/// with its own configuration error instead.
#[test]
fn camera_keeps_its_unconditional_arm_on_apple_targets() {
    for os in ["macos", "ios"] {
        assert_eq!(
            reachable_names(&package_dir("camera"), os),
            vec!["CameraToCudaCopy".to_string()],
            "camera must still declare its cross-platform arm on {os}"
        );
    }
}

/// Behavior delta pinned: `@tatolab/audio` gated its whole `export_plugin!` on
/// `any(linux, macos, ios)`, so a Windows build registered 0 processors even
/// though 5 of the 7 are platform-free. Mirroring each arm's own `#[cfg]`
/// surfaces exactly those 5.
#[test]
fn audio_declares_its_platform_free_arms_on_windows() {
    assert_eq!(
        reachable_names(&package_dir("audio"), "windows"),
        vec![
            "AudioChannelConverter".to_string(),
            "AudioMixer".to_string(),
            "AudioResampler".to_string(),
            "BufferRechunker".to_string(),
            "ChordGenerator".to_string(),
        ],
        "the five platform-free audio processors must survive on Windows"
    );
}

/// `@tatolab/audio` is the live two-arm case: `audio_capture_linux.rs` and
/// `audio_capture_apple.rs` each declare `AudioCapture` (and the two
/// `audio_output_*` siblings `AudioOutput`) under mutually-exclusive gates, kept
/// identical today by copy-paste discipline alone. The scan's divergence check
/// is what turns that discipline into a build gate — the two arms derive one
/// availability entry per processor, which they can only do by agreeing on the
/// whole manifest projection. Mentally change one arm's port name and this
/// scan fails instead of silently shipping a host-dependent `processors:`.
#[test]
fn the_audio_platform_arms_agree_well_enough_to_fold_into_one_processor() {
    let dir = package_dir("audio");
    let capture = availability_of(&dir, "AudioCapture");
    assert_eq!(
        capture.declaring_arm_source_files,
        vec![
            PathBuf::from("processors/audio_capture_apple.rs"),
            PathBuf::from("processors/audio_capture_linux.rs"),
        ],
        "both platform arms must declare `AudioCapture`"
    );
    assert_eq!(
        available_build_target_operating_systems(&capture),
        vec!["linux", "macos", "ios"],
        "availability is the disjunction of the two arms' gates: {:?}",
        capture.availability_cfg_predicate
    );
    // Both arms declare the same `@org/package/Type`, which is what the
    // divergence check enforces before they are allowed to fold into one entry.
    let declared_idents: BTreeSet<String> = extract_processors_across_every_build_target(&dir)
        .expect("scanning @tatolab/audio")
        .processor_declarations
        .iter()
        .filter(|declaration| declaration.schema.name == "AudioCapture")
        .map(|declaration| declaration.schema_ident.to_string())
        .collect();
    assert_eq!(
        declared_idents,
        BTreeSet::from(["@tatolab/audio/AudioCapture@0.0.0".to_string()])
    );

    // Only the target's own arm resolves, so no target registers it twice.
    for os in ["linux", "macos"] {
        let names = reachable_names(&dir, os);
        assert_eq!(names.iter().filter(|n| *n == "AudioCapture").count(), 1);
        assert_eq!(names.iter().filter(|n| *n == "AudioOutput").count(), 1);
    }
}

/// `@tatolab/camera`'s `CameraToCudaCopy` carries no `#[cfg]` at all — only its
/// fields and internals are gated — so it is available on every target and
/// fails in `setup()` off Linux. That is the datum that supersedes the
/// "Linux-only" prose the description string used to carry, and it is what
/// `camera_keeps_its_unconditional_arm_on_apple_targets` observes from the
/// other side.
#[test]
fn the_unconditional_camera_arm_is_available_on_every_build_target() {
    let availability = availability_of(&package_dir("camera"), "CameraToCudaCopy");
    assert!(
        availability.availability_cfg_predicate.is_none(),
        "an unconditional processor carries no availability predicate, got {:?}",
        availability.availability_cfg_predicate
    );
    assert_eq!(
        available_build_target_operating_systems(&availability),
        vec!["linux", "macos", "ios", "windows"]
    );

    // Its Linux-gated sibling is the contrast: a real per-target availability.
    let camera = availability_of(&package_dir("camera"), "Camera");
    assert_eq!(
        available_build_target_operating_systems(&camera),
        vec!["linux"],
        "`Camera`'s only live arm is `processors/camera_linux.rs`: {:?}",
        camera.availability_cfg_predicate
    );
}

/// `@tatolab/clap` is Apple-only: every arm carries the same
/// `any(target_os = "macos", target_os = "ios")` file-level gate, so the
/// generated crate root gates the whole `export_plugin!` on that disjunction
/// and a Linux build emits no declaration — unchanged from the hand-written
/// `#[cfg(any(macos, ios))] export_plugin!(...)` it replaces.
#[test]
fn an_apple_only_package_declares_its_processor_only_on_apple_targets() {
    let dir = package_dir("clap");
    for os in ["macos", "ios"] {
        assert_eq!(
            reachable_names(&dir, os),
            vec!["ClapEffect".to_string()],
            "clap must declare its processor on {os}"
        );
    }
    for os in ["linux", "windows"] {
        assert!(
            reachable_names(&dir, os).is_empty(),
            "clap must declare no processor on {os}"
        );
    }
}

/// `@tatolab/screen-capture`'s only processor is parked, so it declares none on
/// any target — which is why its generated crate root emits no `export_plugin!`
/// and its cdylib carries no `STREAMLIB_PLUGIN` symbol, unchanged from before
/// folder-backed discovery.
#[test]
fn a_fully_parked_package_declares_no_processor_on_any_target() {
    for os in ["linux", "macos", "windows"] {
        assert!(
            reachable_names(&package_dir("screen-capture"), os).is_empty(),
            "screen-capture must declare no processor on {os}"
        );
    }
}

/// `@tatolab/display` has no Apple implementation yet. Its refusal lives as its
/// own arm (`processors/apple_unsupported.rs`, gated on the Apple targets and
/// declaring no processor), so an Apple build fails at compile with the reason
/// instead of producing a cdylib that registers nothing. The arm must be
/// declared by the generated root and must contribute no `export_plugin!` entry.
#[test]
fn display_refuses_an_apple_build_with_a_reason_rather_than_an_empty_plugin() {
    let dir = package_dir("display");
    let refusal_arm = dir.join("processors").join("apple_unsupported.rs");
    let arm_source = std::fs::read_to_string(&refusal_arm)
        .unwrap_or_else(|e| panic!("{} must exist: {e}", refusal_arm.display()));
    assert!(arm_source.contains("#![cfg(any(target_os = \"macos\", target_os = \"ios\"))]"));
    assert!(arm_source.contains("compile_error!"));

    let request = RustCrateRootGenerationRequest::for_package_dir(&dir).unwrap();
    let generated = generate_rust_crate_root_source(&request).unwrap();
    assert!(
        generated.source.contains(
            "#[cfg(any(target_os = \"macos\", target_os = \"ios\"))]\n\
             #[path = \"../processors/apple_unsupported.rs\"]\npub mod apple_unsupported;"
        ),
        "{}",
        generated.source
    );
    for os in ["macos", "ios"] {
        assert!(
            reachable_names(&dir, os).is_empty(),
            "the refusal arm must declare no processor on {os}"
        );
    }
}

/// `runtime/streamlib-api-server` is the one in-tree Rust crate that is a
/// statically linked host rlib rather than a distributable cdylib, so it keeps
/// a committed `src/lib.rs` crate root. It still authors its processor under
/// `processors/` — `processors/` is the one discovery root for every crate-type
/// — so its committed `processors:` stays backed by code rather than deriving
/// empty. It lives in the engine tree, not `packages/`: it is infrastructure a
/// host links, never something the package source distributes.
#[test]
fn the_host_rlib_crate_still_derives_its_processor_from_the_shared_root() {
    let dir = engine_crate_dir("streamlib-api-server");
    assert!(
        !RustCrateRootGenerationRequest::for_package_dir(&dir)
            .unwrap()
            .emits_plugin_export_envelope,
        "api-server is a host rlib — if it ever ships a cdylib it must move to the \
         generated crate root like every other distributable package"
    );
    for os in ["linux", "macos"] {
        assert_eq!(
            reachable_names(&dir, os),
            vec!["ApiServer".to_string()],
            "api-server must derive its committed `ApiServer` processor on {os}"
        );
    }
}

/// The generated register list and the derived `processors:` manifest come out
/// of two different walks — `extract_processors_across_every_build_target` plus
/// verbatim cfg mirroring for the crate root, `extract_reachable_rust_processors`
/// for the manifest. This is the invariant the whole change rests on: for a
/// given target, the `export_plugin!` entries that survive cfg-stripping are
/// exactly the processors the manifest walk resolves. Nothing else in-tree
/// stops the two from disagreeing.
#[test]
fn the_generated_register_list_equals_the_reachable_set_on_every_target() {
    let packages = discover_package_dirs_declaring_a_generated_crate_root(&workspace_root())
        .expect("discovering folder-backed packages");
    assert!(
        packages.len() >= 15,
        "expected the in-tree folder-backed packages; found {packages:?}"
    );

    for package in &packages {
        let request = RustCrateRootGenerationRequest::for_package_dir(package).unwrap();
        if !request.emits_plugin_export_envelope {
            continue;
        }
        let generated = generate_rust_crate_root_source(&request).unwrap();
        for os in ["linux", "macos", "ios", "windows"] {
            let target = target_for(os);
            let registered = registered_export_entries(&generated.source, &target);
            let reachable: BTreeSet<String> = extract_reachable_rust_processors(package, &target)
                .unwrap()
                .into_iter()
                .map(|p| {
                    let mut segments = p.module_path_segments;
                    segments.push(p.struct_name);
                    segments.push("Processor".to_string());
                    format!("crate::{}", segments.join("::"))
                })
                .collect();
            assert_eq!(
                registered,
                reachable,
                "{} on {os}: the generated `export_plugin!` register list and the \
                 reachability-resolved manifest set must be the same set",
                package.display()
            );
        }
    }
}

/// Every in-tree folder-backed package must pass the id-grouping checks — the
/// regression net that says the overlap and divergence rules do not
/// false-positive on real source.
///
/// The two checks fire from different seams and neither subsumes the other:
/// `extract_processors_across_every_build_target` proves overlap out of the
/// arms' own predicates and compares their whole manifest projections, while
/// `extract_reachable_rust_processors` resolves one concrete target and refuses
/// a `Type` it collected twice there. A package is only clean when both agree,
/// on every target it might be built for.
#[test]
fn every_in_tree_package_passes_the_processor_id_grouping_checks() {
    let packages = discover_package_dirs_declaring_a_generated_crate_root(&workspace_root())
        .expect("discovering folder-backed packages");
    assert!(
        packages.len() >= 15,
        "expected the in-tree folder-backed packages; found {packages:?}"
    );

    for package in &packages {
        let set = extract_processors_across_every_build_target(package)
            .unwrap_or_else(|e| panic!("{}: {e}", package.display()));

        // Every declaration folds into exactly one availability entry, and no
        // two entries share a `Type` — the `processors:` collision key.
        let distinct_type_names: BTreeSet<&str> = set
            .processor_declarations
            .iter()
            .map(|declaration| declaration.schema.name.as_str())
            .collect();
        assert_eq!(
            set.processor_availability.len(),
            distinct_type_names.len(),
            "{}: one availability entry per distinct processor `Type`",
            package.display()
        );

        for os in ["linux", "macos", "ios", "windows"] {
            let target = target_for(os);
            let reachable: Vec<String> =
                sorted_names(extract_reachable_rust_processors(package, &target).unwrap());
            let available: Vec<String> = {
                let mut names: Vec<String> = set
                    .processor_availability
                    .iter()
                    .filter(|entry| entry.is_available_on_build_target(&target))
                    .map(|entry| entry.processor_type_name.clone())
                    .collect();
                names.sort();
                names
            };
            assert_eq!(
                reachable,
                available,
                "{} on {os}: the availability predicates and the target-resolved \
                 scan must name the same processors",
                package.display()
            );
        }
    }
}

/// The `crate::…::Processor` paths a generated crate root's `export_plugin!`
/// registers on `target`, honoring both the invocation's outer `#[cfg]` and each
/// entry's own. Parses the generator's rendered text rather than its inputs —
/// the point is to compare what actually reaches `rustc`.
fn registered_export_entries(
    generated_source: &str,
    target: &ModuleReachabilityTarget,
) -> BTreeSet<String> {
    let mut entries = BTreeSet::new();
    let lines: Vec<&str> = generated_source.lines().collect();
    let Some(invocation) = lines
        .iter()
        .position(|line| line.starts_with("streamlib_plugin_abi::export_plugin!("))
    else {
        return entries;
    };
    if let Some(outer_gate) = invocation
        .checked_sub(1)
        .and_then(|i| cfg_predicate_of(lines[i]))
        && !target.cfg_predicate_source_holds(&outer_gate)
    {
        return entries;
    }

    let mut pending_entry_gate: Option<String> = None;
    for line in &lines[invocation + 1..] {
        if line.starts_with(");") {
            break;
        }
        match cfg_predicate_of(line.trim()) {
            Some(predicate) => pending_entry_gate = Some(predicate),
            None => {
                let type_path = line.trim().trim_end_matches(',').to_string();
                let gated_out = pending_entry_gate
                    .take()
                    .is_some_and(|predicate| !target.cfg_predicate_source_holds(&predicate));
                if !gated_out {
                    entries.insert(type_path);
                }
            }
        }
    }
    entries
}

/// The predicate inside a rendered `#[cfg(<predicate>)]` line, if the line is one.
fn cfg_predicate_of(line: &str) -> Option<String> {
    let inner = line.trim().strip_prefix("#[cfg(")?.strip_suffix(")]")?;
    Some(inner.to_string())
}

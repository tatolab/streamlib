// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::*;

const INDEX_URL: &str = "sparse+http://localhost:3300/api/packages/tatolab/cargo/";

/// Absolute path of the real streamlib workspace this test binary was built in
/// (`<root>/libs/streamlib-cli` → `<root>`). It is a genuine streamlib
/// checkout, so checkout-facing paths exercise real behavior offline.
fn workspace_checkout() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root above libs/streamlib-cli")
        .to_path_buf()
}

/// A consumer dir carrying its own gitea registry config, a pyproject, and a
/// deno.json — the three manifest types link mode overrides.
fn write_full_consumer(root: &Path) {
    let cargo_dir = root.join(".cargo");
    std::fs::create_dir_all(&cargo_dir).unwrap();
    std::fs::write(
        cargo_dir.join("config.toml"),
        format!(
            "# consumer cargo config\n[registries.gitea]\nindex = \"{INDEX_URL}\"\n\n[alias]\nb = \"build\"\n"
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("pyproject.toml"),
        "[project]\nname = \"consumer\"\nversion = \"0.1.0\"\ndependencies = [\"streamlib\"]\n",
    )
    .unwrap();
    std::fs::write(
        root.join("deno.json"),
        "{\n  \"imports\": {\n    \"streamlib\": \"npm:@tatolab/streamlib-deno@^0.4\"\n  }\n}\n",
    )
    .unwrap();
}

fn fake_crate_set() -> BTreeMap<String, PathBuf> {
    let mut m = BTreeMap::new();
    m.insert(
        "streamlib".to_string(),
        PathBuf::from("/checkout/libs/streamlib-sdk"),
    );
    m.insert(
        "streamlib-idents".to_string(),
        PathBuf::from("/checkout/libs/streamlib-idents"),
    );
    m
}

/// Drive the full transaction (manifest-first, apply, flip) with a fixed
/// crate set — the real flow minus cargo-metadata derivation + verification.
fn link_with_fixed_crates(consumer_root: &Path, checkout: &Path) {
    establish_link(consumer_root, checkout, INDEX_URL, &fake_crate_set()).unwrap();
}

fn marker_path(consumer: &Path) -> PathBuf {
    consumer.join(LINK_STATE_DIR).join(LINK_MANIFEST_FILE)
}

#[test]
fn link_then_unlink_is_byte_clean_and_idempotent_across_cycles() {
    let tmp = tempfile::tempdir().unwrap();
    let consumer = tmp.path().canonicalize().unwrap();
    write_full_consumer(&consumer);

    let cargo = consumer.join(".cargo").join("config.toml");
    let pyproject = consumer.join("pyproject.toml");
    let deno = consumer.join("deno.json");
    let orig_cargo = std::fs::read(&cargo).unwrap();
    let orig_py = std::fs::read(&pyproject).unwrap();
    let orig_deno = std::fs::read(&deno).unwrap();

    let checkout = PathBuf::from("/some/checkout");

    for cycle in 0..2 {
        link_with_fixed_crates(&consumer, &checkout);

        // During link: overrides present, marker present + flipped to active.
        let manifest = load_active_manifest(&consumer).unwrap().unwrap();
        assert_eq!(manifest.state, LinkTransactionState::Active, "cycle {cycle}");
        assert_eq!(
            manifest.python_sdk_path,
            PathBuf::from("/some/checkout/libs/streamlib-python"),
            "cycle {cycle}: manifest must carry the resolved absolute python sdk path"
        );
        let linked_cargo = std::fs::read_to_string(&cargo).unwrap();
        assert!(
            linked_cargo.contains(&format!("[patch.\"{INDEX_URL}\"]")),
            "cycle {cycle}: cargo config must carry the patch section:\n{linked_cargo}"
        );
        assert!(linked_cargo.contains("streamlib-idents"));
        assert!(linked_cargo.contains(CARGO_PATCH_MARKER));
        assert!(linked_cargo.contains("[registries.gitea]"));
        assert!(std::fs::read_to_string(&pyproject)
            .unwrap()
            .contains("[tool.uv.sources]"));
        assert!(std::fs::read_to_string(&deno)
            .unwrap()
            .contains("libs/streamlib-deno/mod.ts"));

        unlink(&consumer, false).unwrap();

        // After unlink: byte-identical + zero residue.
        assert_eq!(std::fs::read(&cargo).unwrap(), orig_cargo, "cycle {cycle}: cargo");
        assert_eq!(std::fs::read(&pyproject).unwrap(), orig_py, "cycle {cycle}: pyproject");
        assert_eq!(std::fs::read(&deno).unwrap(), orig_deno, "cycle {cycle}: deno");
        assert!(
            !consumer.join(LINK_STATE_DIR).exists(),
            "cycle {cycle}: .streamlib state must be gone"
        );
    }
}

#[test]
fn unlink_deletes_a_cargo_config_it_created_and_prunes_the_dir() {
    // Registry index lives in a PARENT dir; the consumer has no .cargo of its
    // own, so link CREATES consumer/.cargo/config.toml and unlink must remove
    // it and prune the empty .cargo dir.
    let tmp = tempfile::tempdir().unwrap();
    let outer = tmp.path().canonicalize().unwrap();
    let outer_cargo = outer.join(".cargo");
    std::fs::create_dir_all(&outer_cargo).unwrap();
    std::fs::write(
        outer_cargo.join("config.toml"),
        format!("[registries.gitea]\nindex = \"{INDEX_URL}\"\n"),
    )
    .unwrap();

    let consumer = outer.join("app");
    std::fs::create_dir_all(&consumer).unwrap();

    // Discovery must find the parent's index (no home fallback involved).
    assert_eq!(
        discover_registry_index_with_home(&consumer, None).unwrap(),
        INDEX_URL
    );

    link_with_fixed_crates(&consumer, &PathBuf::from("/checkout"));
    assert!(consumer.join(".cargo").join("config.toml").is_file());

    unlink(&consumer, false).unwrap();
    assert!(!consumer.join(".cargo").exists(), ".cargo we created must be pruned");
    assert!(!consumer.join(LINK_STATE_DIR).exists());
    // Parent config untouched.
    assert!(outer_cargo.join("config.toml").is_file());
}

#[test]
fn unlink_with_no_active_link_is_a_friendly_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let consumer = tmp.path().canonicalize().unwrap();
    unlink(&consumer, false).expect("unlink with no link must be Ok");
}

#[test]
fn link_to_a_nonexistent_checkout_modifies_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let consumer = tmp.path().canonicalize().unwrap();
    write_full_consumer(&consumer);
    let before = std::fs::read(consumer.join(".cargo").join("config.toml")).unwrap();

    let err = link(&consumer, &consumer.join("does-not-exist"), true).unwrap_err();
    assert!(matches!(err, LinkError::NotAStreamlibCheckout(_)), "got {err:?}");
    assert!(!consumer.join(LINK_STATE_DIR).exists());
    assert_eq!(
        std::fs::read(consumer.join(".cargo").join("config.toml")).unwrap(),
        before
    );
}

#[test]
fn link_to_a_dir_without_cargo_toml_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let consumer = tmp.path().canonicalize().unwrap();
    write_full_consumer(&consumer);
    let bogus = tempfile::tempdir().unwrap();
    let err = link(&consumer, bogus.path(), true).unwrap_err();
    assert!(matches!(err, LinkError::NotAStreamlibCheckout(_)), "got {err:?}");
}

#[test]
fn missing_registry_index_errors_actionably() {
    // Injectable home (None) makes this assertion environment-independent.
    let tmp = tempfile::tempdir().unwrap();
    let consumer = tmp.path().canonicalize().unwrap();
    let cargo = consumer.join(".cargo");
    std::fs::create_dir_all(&cargo).unwrap();
    std::fs::write(cargo.join("config.toml"), "[alias]\nb = \"build\"\n").unwrap();

    let err = discover_registry_index_with_home(&consumer, None);
    assert!(
        matches!(err, Err(LinkError::RegistryIndexNotConfigured)),
        "got {err:?}"
    );
}

#[test]
fn pyproject_and_deno_overrides_are_presence_gated() {
    let tmp = tempfile::tempdir().unwrap();
    let consumer = tmp.path().canonicalize().unwrap();
    // Cargo config only — no pyproject, no deno.json.
    let cargo = consumer.join(".cargo");
    std::fs::create_dir_all(&cargo).unwrap();
    std::fs::write(
        cargo.join("config.toml"),
        format!("[registries.gitea]\nindex = \"{INDEX_URL}\"\n"),
    )
    .unwrap();

    let edits = plan_edits(&consumer, &PathBuf::from("/checkout"), INDEX_URL, &fake_crate_set()).unwrap();
    assert_eq!(edits.len(), 1, "only the cargo config should be planned");
    assert_eq!(edits[0].rel_path, PathBuf::from(".cargo").join("config.toml"));
}

#[test]
fn linking_a_different_checkout_over_an_active_link_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let consumer = tmp.path().canonicalize().unwrap();
    write_full_consumer(&consumer);
    link_with_fixed_crates(&consumer, &PathBuf::from("/checkout-A"));

    let err = link(&consumer, &workspace_checkout(), true).unwrap_err();
    assert!(matches!(err, LinkError::AlreadyLinkedElsewhere { .. }), "got {err:?}");
}

#[test]
fn deno_jsonc_with_comments_is_rejected_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    let consumer = tmp.path().canonicalize().unwrap();
    let cargo = consumer.join(".cargo");
    std::fs::create_dir_all(&cargo).unwrap();
    std::fs::write(
        cargo.join("config.toml"),
        format!("[registries.gitea]\nindex = \"{INDEX_URL}\"\n"),
    )
    .unwrap();
    std::fs::write(
        consumer.join("deno.jsonc"),
        "{\n  // a comment breaks strict JSON\n  \"imports\": {}\n}\n",
    )
    .unwrap();
    let err = plan_edits(&consumer, &PathBuf::from("/checkout"), INDEX_URL, &fake_crate_set())
        .unwrap_err();
    assert!(matches!(err, LinkError::ManifestParse { .. }), "got {err:?}");
}

#[test]
fn non_table_pyproject_tool_key_is_a_typed_error_not_a_panic() {
    // `tool = 3` occupies the key with a non-table — planning must return
    // ManifestParse, never panic on an unchecked as_table_mut.
    let tmp = tempfile::tempdir().unwrap();
    let consumer = tmp.path().canonicalize().unwrap();
    let pyproject = consumer.join("pyproject.toml");
    std::fs::write(&pyproject, "tool = 3\n").unwrap();
    let err = plan_pyproject_edit(&pyproject, &consumer, &PathBuf::from("/checkout")).unwrap_err();
    assert!(matches!(err, LinkError::ManifestParse { .. }), "got {err:?}");
}

#[test]
fn concurrent_second_link_is_refused_by_exclusive_marker_create() {
    // Simulates the race window after the caller's existing-manifest check:
    // a marker landed in between — the exclusive create must refuse.
    let tmp = tempfile::tempdir().unwrap();
    let consumer = tmp.path().canonicalize().unwrap();
    write_full_consumer(&consumer);
    let state_dir = consumer.join(LINK_STATE_DIR);
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::write(state_dir.join(LINK_MANIFEST_FILE), "{}").unwrap();

    let err = establish_link(&consumer, &PathBuf::from("/checkout"), INDEX_URL, &fake_crate_set())
        .unwrap_err();
    assert!(matches!(err, LinkError::LinkMarkerAlreadyExists { .. }), "got {err:?}");
}

#[test]
fn torn_applying_state_refuses_relink_and_plain_unlink_recovers() {
    // Crash window (a): manifest persisted (state=applying), NO edits applied.
    let tmp = tempfile::tempdir().unwrap();
    let consumer = tmp.path().canonicalize().unwrap();
    write_full_consumer(&consumer);
    let cargo = consumer.join(".cargo").join("config.toml");
    let orig_cargo = std::fs::read(&cargo).unwrap();

    // Torn state against a real (canonicalizable) checkout so `link()` reaches
    // the state gate.
    let real_checkout = workspace_checkout();
    let edits = plan_edits(&consumer, &real_checkout, INDEX_URL, &fake_crate_set()).unwrap();
    let manifest = build_link_manifest(&real_checkout, 2, &edits);
    write_manifest_excl(&consumer, &manifest).unwrap();
    // (crash here — no edits, no backups)

    let err = link(&consumer, &real_checkout, true).unwrap_err();
    assert!(matches!(err, LinkError::TornLinkState { .. }), "got {err:?}");

    // Plain unlink recovers: all files still pre-edit → skipped; state gone.
    unlink(&consumer, false).unwrap();
    assert_eq!(std::fs::read(&cargo).unwrap(), orig_cargo);
    assert!(!consumer.join(LINK_STATE_DIR).exists());
}

#[test]
fn crash_after_edits_before_state_flip_is_recovered_by_unlink() {
    // Crash window (b): manifest persisted, ALL edits applied, state never
    // flipped to active.
    let tmp = tempfile::tempdir().unwrap();
    let consumer = tmp.path().canonicalize().unwrap();
    write_full_consumer(&consumer);
    let cargo = consumer.join(".cargo").join("config.toml");
    let orig_cargo = std::fs::read(&cargo).unwrap();

    let checkout = PathBuf::from("/checkout");
    let edits = plan_edits(&consumer, &checkout, INDEX_URL, &fake_crate_set()).unwrap();
    let manifest = build_link_manifest(&checkout, 2, &edits);
    write_manifest_excl(&consumer, &manifest).unwrap();
    apply_transaction(&consumer, &edits).unwrap();
    // (crash here — state still `applying`)

    assert_eq!(
        load_active_manifest(&consumer).unwrap().unwrap().state,
        LinkTransactionState::Applying
    );
    unlink(&consumer, false).unwrap();
    assert_eq!(std::fs::read(&cargo).unwrap(), orig_cargo);
    assert!(!consumer.join(LINK_STATE_DIR).exists());
}

#[test]
fn apply_failure_on_the_last_edit_rolls_back_earlier_edits_byte_identically() {
    // T1: force the LAST planned edit (deno.json) to fail via read-only
    // permissions; cargo + pyproject edits must roll back byte-identically
    // and the verified rollback clears all link state.
    let tmp = tempfile::tempdir().unwrap();
    let consumer = tmp.path().canonicalize().unwrap();
    write_full_consumer(&consumer);
    let cargo = consumer.join(".cargo").join("config.toml");
    let pyproject = consumer.join("pyproject.toml");
    let deno = consumer.join("deno.json");
    let orig_cargo = std::fs::read(&cargo).unwrap();
    let orig_py = std::fs::read(&pyproject).unwrap();
    let orig_deno = std::fs::read(&deno).unwrap();

    // Make deno.json unwritable. Skip when the environment ignores file modes
    // (running as root), probed by attempting the write.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&deno, std::fs::Permissions::from_mode(0o444)).unwrap();
    }
    if std::fs::write(&deno, &orig_deno).is_ok() {
        eprintln!("skipping: file modes not enforced (running as root?)");
        return;
    }

    let err = establish_link(&consumer, &PathBuf::from("/checkout"), INDEX_URL, &fake_crate_set())
        .unwrap_err();
    assert!(matches!(err, LinkError::Io { .. }), "got {err:?}");

    // Earlier edits rolled back byte-identically; failing target unmodified;
    // verified rollback cleared the state dir.
    assert_eq!(std::fs::read(&cargo).unwrap(), orig_cargo);
    assert_eq!(std::fs::read(&pyproject).unwrap(), orig_py);
    assert_eq!(std::fs::read(&deno).unwrap(), orig_deno);
    assert!(!consumer.join(LINK_STATE_DIR).exists());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&deno, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
}

#[test]
fn incomplete_rollback_preserves_backups_and_link_state() {
    // Dir-occupation on deno.json between plan and apply: the apply fails AND
    // the rollback cannot restore that file → RollbackIncomplete, state dir
    // (manifest + backups) preserved for `streamlib unlink` recovery.
    let tmp = tempfile::tempdir().unwrap();
    let consumer = tmp.path().canonicalize().unwrap();
    write_full_consumer(&consumer);
    let cargo = consumer.join(".cargo").join("config.toml");
    let orig_cargo = std::fs::read(&cargo).unwrap();
    let deno = consumer.join("deno.json");

    let checkout = PathBuf::from("/checkout");
    let edits = plan_edits(&consumer, &checkout, INDEX_URL, &fake_crate_set()).unwrap();
    let manifest = build_link_manifest(&checkout, 2, &edits);
    write_manifest_excl(&consumer, &manifest).unwrap();

    // Occupy deno.json with a directory — the write fails and so does the
    // rollback write.
    std::fs::remove_file(&deno).unwrap();
    std::fs::create_dir(&deno).unwrap();

    let apply_err = apply_transaction(&consumer, &edits).unwrap_err();
    let err = unwind_failed_transaction(&consumer, &edits, apply_err);
    assert!(matches!(err, LinkError::RollbackIncomplete { .. }), "got {err:?}");

    // Earlier edits still restored; backups + manifest preserved.
    assert_eq!(std::fs::read(&cargo).unwrap(), orig_cargo);
    assert!(marker_path(&consumer).is_file(), "link state must be preserved");
    assert!(
        consumer
            .join(LINK_STATE_DIR)
            .join(LINK_BACKUP_DIR)
            .join("deno.json")
            .is_file(),
        "the failing file's backup must be preserved"
    );
}

#[test]
fn unlink_tristate_refuses_user_edits_without_force_and_restores_with_it() {
    let tmp = tempfile::tempdir().unwrap();
    let consumer = tmp.path().canonicalize().unwrap();
    write_full_consumer(&consumer);
    let cargo = consumer.join(".cargo").join("config.toml");
    let orig_cargo = std::fs::read(&cargo).unwrap();

    link_with_fixed_crates(&consumer, &PathBuf::from("/checkout"));

    // User hand-edits the linked cargo config during the link window.
    let mut edited = std::fs::read(&cargo).unwrap();
    edited.extend_from_slice(b"\n# user edit during link window\n");
    std::fs::write(&cargo, &edited).unwrap();

    // Without --force: refuse, tree untouched.
    let err = unlink(&consumer, false).unwrap_err();
    assert!(matches!(err, LinkError::UnlinkRefusedModifiedFile { .. }), "got {err:?}");
    assert_eq!(std::fs::read(&cargo).unwrap(), edited, "refusal must not mutate");
    assert!(marker_path(&consumer).is_file(), "link state preserved on refusal");

    // With --force: discard the user edit, restore the pre-link original.
    unlink(&consumer, true).unwrap();
    assert_eq!(std::fs::read(&cargo).unwrap(), orig_cargo);
    assert!(!consumer.join(LINK_STATE_DIR).exists());
}

#[test]
fn unlink_skips_files_the_user_already_reverted() {
    let tmp = tempfile::tempdir().unwrap();
    let consumer = tmp.path().canonicalize().unwrap();
    write_full_consumer(&consumer);
    let cargo = consumer.join(".cargo").join("config.toml");
    let orig_cargo = std::fs::read(&cargo).unwrap();

    link_with_fixed_crates(&consumer, &PathBuf::from("/checkout"));

    // User manually reverts the cargo config to the original bytes.
    std::fs::write(&cargo, &orig_cargo).unwrap();

    // Unlink skips it (live == pre-edit hash) and still restores the rest.
    unlink(&consumer, false).unwrap();
    assert_eq!(std::fs::read(&cargo).unwrap(), orig_cargo);
    assert!(!consumer.join(LINK_STATE_DIR).exists());
}

#[test]
fn unlink_refuses_to_restore_a_corrupted_backup() {
    let tmp = tempfile::tempdir().unwrap();
    let consumer = tmp.path().canonicalize().unwrap();
    write_full_consumer(&consumer);
    link_with_fixed_crates(&consumer, &PathBuf::from("/checkout"));

    // Tamper with the cargo config backup so its hash no longer matches.
    let backup = consumer
        .join(LINK_STATE_DIR)
        .join(LINK_BACKUP_DIR)
        .join(".cargo")
        .join("config.toml");
    std::fs::write(&backup, b"tampered content").unwrap();

    let err = unlink(&consumer, false).unwrap_err();
    assert!(matches!(err, LinkError::CorruptLinkState { .. }), "got {err:?}");
    // Refused restore ⇒ the live (linked) config was NOT clobbered.
    let live = std::fs::read_to_string(consumer.join(".cargo").join("config.toml")).unwrap();
    assert!(live.contains("[patch."), "live config must be untouched by the refused restore");
}

#[test]
fn failed_refresh_derivation_preserves_the_active_link() {
    // F7 ordering lock: on a same-checkout refresh, crate-set derivation runs
    // BEFORE the unlink — a checkout whose metadata breaks between links must
    // leave the working link fully intact.
    let tmp = tempfile::tempdir().unwrap();
    let consumer = tmp.path().canonicalize().unwrap();
    write_full_consumer(&consumer);

    // A "checkout" that canonicalizes + has a Cargo.toml, but whose metadata
    // is broken.
    let broken = tempfile::tempdir().unwrap();
    std::fs::write(broken.path().join("Cargo.toml"), "not toml at all [").unwrap();
    let broken_checkout = broken.path().canonicalize().unwrap();

    link_with_fixed_crates(&consumer, &broken_checkout);
    let linked_cargo = std::fs::read(consumer.join(".cargo").join("config.toml")).unwrap();

    let err = link(&consumer, &broken_checkout, true).unwrap_err();
    assert!(matches!(err, LinkError::CrateSetDerivation { .. }), "got {err:?}");

    // The active link survived the failed refresh.
    assert!(marker_path(&consumer).is_file(), "link state must survive");
    assert_eq!(
        std::fs::read(consumer.join(".cargo").join("config.toml")).unwrap(),
        linked_cargo,
        "linked cargo config must be untouched by the failed refresh"
    );
    assert_eq!(
        load_active_manifest(&consumer).unwrap().unwrap().state,
        LinkTransactionState::Active
    );
}

#[test]
fn derive_linkable_crates_selects_the_streamlib_sdk_closure() {
    // Runs `cargo metadata --no-deps` against the real workspace this test was
    // built in — offline, no registry required.
    let checkout = workspace_checkout();
    let crates = match derive_linkable_crates(&checkout) {
        Ok(c) => c,
        Err(LinkError::CrateSetDerivation { detail, .. }) if detail.contains("failed to run cargo") => {
            eprintln!("skipping: cargo not available to run metadata");
            return;
        }
        Err(e) => panic!("cargo metadata failed: {e}"),
    };

    let sdk = crates.get("streamlib").expect("the `streamlib` SDK crate must be linkable");
    assert!(
        sdk.ends_with("libs/streamlib-sdk"),
        "streamlib SDK member dir should be libs/streamlib-sdk, got {}",
        sdk.display()
    );
    assert!(crates.contains_key("streamlib-idents"));
    assert!(
        crates.len() > 10,
        "the whole-tree closure should be broad, got {} crates",
        crates.len()
    );
    // Binaries and test fixtures are excluded (no lib target / publish=false).
    assert!(!crates.contains_key("streamlib-cli"));
}

#[test]
fn real_link_offline_e2e_link_refresh_unlink_roundtrip() {
    // T3: the real `link()` composition path — index discovery, live crate
    // derivation, transactional emission, post-link cargo verification,
    // same-checkout refresh (derive-before-unlink), byte-clean unlink —
    // against the actual workspace checkout, fully offline.
    let checkout = workspace_checkout();
    let tmp = tempfile::tempdir().unwrap();
    let consumer = tmp.path().canonicalize().unwrap();
    let cargo_dir = consumer.join(".cargo");
    std::fs::create_dir_all(&cargo_dir).unwrap();
    std::fs::write(
        cargo_dir.join("config.toml"),
        format!("[registries.gitea]\nindex = \"{INDEX_URL}\"\n"),
    )
    .unwrap();
    std::fs::create_dir_all(consumer.join("src")).unwrap();
    std::fs::write(consumer.join("src").join("main.rs"), "fn main(){}\n").unwrap();
    std::fs::write(
        consumer.join("Cargo.toml"),
        "[package]\nname = \"link-e2e-consumer\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
         publish = false\n[dependencies]\nstreamlib-idents = { version = \"0.5\", registry = \"gitea\" }\n[workspace]\n",
    )
    .unwrap();
    let orig_config = std::fs::read(cargo_dir.join("config.toml")).unwrap();

    match link(&consumer, &checkout, false) {
        Ok(()) => {}
        // Environment-dependent skips: no cargo, or a cold cargo cache that
        // can't resolve offline AND no registry online.
        Err(LinkError::CrateSetDerivation { detail, .. })
            if detail.contains("failed to run cargo") =>
        {
            eprintln!("skipping: cargo unavailable");
            return;
        }
        Err(LinkError::LinkVerificationFailed { detail }) => {
            eprintln!("skipping: offline verification not possible in this environment: {detail}");
            return;
        }
        Err(e) => panic!("real link() failed: {e}"),
    }

    let linked = std::fs::read_to_string(cargo_dir.join("config.toml")).unwrap();
    assert!(linked.contains("[patch."), "patch section must be emitted:\n{linked}");
    assert!(linked.contains("libs/streamlib-sdk"), "sdk path must appear");

    // Same-checkout re-link = refresh (exercises derive-before-unlink + the
    // full re-emission).
    link(&consumer, &checkout, false).expect("same-checkout refresh must succeed");
    assert_eq!(
        load_active_manifest(&consumer).unwrap().unwrap().state,
        LinkTransactionState::Active
    );

    unlink(&consumer, false).unwrap();
    assert_eq!(
        std::fs::read(cargo_dir.join("config.toml")).unwrap(),
        orig_config,
        "cargo config must be byte-identical after unlink"
    );
    assert!(!consumer.join(LINK_STATE_DIR).exists());
    // Cargo.lock may be created by the verification resolve — it's a build
    // artifact of the consumer, not a link-managed manifest.
}

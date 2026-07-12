// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::*;

const INDEX_URL: &str = "sparse+http://localhost:3300/api/packages/tatolab/cargo/";

/// Absolute path of the real streamlib workspace this test binary was built in
/// (`<root>/libs/streamlib-cli` → `<root>`). It is a genuine streamlib
/// checkout, so `derive_linkable_crates` and `canonicalize_checkout` exercise
/// real behavior offline.
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

/// Drive the emission internals with a fixed crate set (no cargo metadata), so
/// the transactional apply + teardown are exercised deterministically.
fn link_with_fixed_crates(consumer_root: &Path, checkout: &Path) {
    let crates = fake_crate_set();
    let edits = plan_edits(consumer_root, checkout, INDEX_URL, &crates).unwrap();
    let touched = apply_transaction(consumer_root, &edits).unwrap();
    write_manifest(
        consumer_root,
        &LinkManifest {
            checkout: checkout.to_path_buf(),
            linked_at: "2026-01-01T00:00:00Z".to_string(),
            linked_crate_count: crates.len(),
            files: touched,
        },
    )
    .unwrap();
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

        // During link: overrides present, marker present.
        assert!(consumer.join(LINK_STATE_DIR).join(LINK_MANIFEST_FILE).is_file());
        let linked_cargo = std::fs::read_to_string(&cargo).unwrap();
        assert!(
            linked_cargo.contains(&format!("[patch.\"{INDEX_URL}\"]")),
            "cycle {cycle}: cargo config must carry the patch section:\n{linked_cargo}"
        );
        assert!(linked_cargo.contains("streamlib-idents"));
        assert!(linked_cargo.contains(CARGO_PATCH_MARKER));
        // The pre-existing registry block survives.
        assert!(linked_cargo.contains("[registries.gitea]"));
        assert!(std::fs::read_to_string(&pyproject)
            .unwrap()
            .contains("[tool.uv.sources]"));
        assert!(std::fs::read_to_string(&deno)
            .unwrap()
            .contains("libs/streamlib-deno/mod.ts"));

        unlink(&consumer).unwrap();

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

    // Discovery must find the parent's index.
    assert_eq!(discover_registry_index(&consumer).unwrap(), INDEX_URL);

    link_with_fixed_crates(&consumer, &PathBuf::from("/checkout"));
    assert!(consumer.join(".cargo").join("config.toml").is_file());

    unlink(&consumer).unwrap();
    assert!(!consumer.join(".cargo").exists(), ".cargo we created must be pruned");
    assert!(!consumer.join(LINK_STATE_DIR).exists());
    // Parent config untouched.
    assert!(outer_cargo.join("config.toml").is_file());
}

#[test]
fn unlink_with_no_active_link_is_a_friendly_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let consumer = tmp.path().canonicalize().unwrap();
    // No .streamlib at all.
    unlink(&consumer).expect("unlink with no link must be Ok");
}

#[test]
fn link_to_a_nonexistent_checkout_modifies_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let consumer = tmp.path().canonicalize().unwrap();
    write_full_consumer(&consumer);
    let before = std::fs::read(consumer.join(".cargo").join("config.toml")).unwrap();

    let err = link(&consumer, &consumer.join("does-not-exist")).unwrap_err();
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
    let err = link(&consumer, bogus.path()).unwrap_err();
    assert!(matches!(err, LinkError::NotAStreamlibCheckout(_)), "got {err:?}");
}

#[test]
fn missing_registry_index_errors_actionably() {
    let tmp = tempfile::tempdir().unwrap();
    let consumer = tmp.path().canonicalize().unwrap();
    // A consumer with a cargo config but NO gitea registry.
    let cargo = consumer.join(".cargo");
    std::fs::create_dir_all(&cargo).unwrap();
    std::fs::write(cargo.join("config.toml"), "[alias]\nb = \"build\"\n").unwrap();
    // Discovery from here should fail (assuming ~/.cargo has no gitea either;
    // if it does, the walk finds it — so only assert the walk-local negative).
    let err = discover_registry_index(&consumer);
    // On dev boxes ~/.cargo may define gitea; only assert when it doesn't.
    if dirs::home_dir()
        .map(|h| read_gitea_index(&h.join(".cargo/config.toml")).ok().flatten().is_none())
        .unwrap_or(true)
    {
        assert!(matches!(err, Err(LinkError::RegistryIndexNotConfigured)), "got {err:?}");
    }
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
    // Simulate an active link to checkout A.
    link_with_fixed_crates(&consumer, &PathBuf::from("/checkout-A"));

    // link() to a DIFFERENT real checkout must refuse (the workspace root is a
    // valid checkout, distinct from /checkout-A).
    let err = link(&consumer, &workspace_checkout()).unwrap_err();
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
fn pack_is_refused_while_a_link_is_active_above_the_package() {
    let tmp = tempfile::tempdir().unwrap();
    let consumer = tmp.path().canonicalize().unwrap();
    write_full_consumer(&consumer);
    link_with_fixed_crates(&consumer, &PathBuf::from("/checkout"));

    // From the consumer root and from a nested package dir, pack must refuse.
    assert!(matches!(
        ensure_no_active_link_for_pack(&consumer),
        Err(LinkError::PackRefusedWhileLinked { .. })
    ));
    let nested = consumer.join("packages").join("thing");
    std::fs::create_dir_all(&nested).unwrap();
    assert!(matches!(
        ensure_no_active_link_for_pack(&nested),
        Err(LinkError::PackRefusedWhileLinked { .. })
    ));

    // After unlink, pack is allowed again.
    unlink(&consumer).unwrap();
    assert!(ensure_no_active_link_for_pack(&consumer).is_ok());
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

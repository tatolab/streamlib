// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Bans building an iceoryx2 node anywhere but the body of the engine's one
//! node constructor in `iceoryx2/node.rs`, building a publish-subscribe service
//! anywhere but that file, and reading iceoryx2's global configuration anywhere
//! at all, that file included.
//!
//! A node built any other way reads iceoryx2's ambient configuration and lands
//! in a different domain from every engine-owned node, where no data flows and
//! nothing errors — a partial migration hangs tests silently. A channel data
//! service built any other way lacks the sequence-number user header every
//! opener must present, so it either fails to open against the engine's
//! channels or carries bags no destination can count the loss of. So unlike
//! every other gate, test modules, test files and benches are scanned too.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// The repo-relative roots holding Rust that can reach iceoryx2.
const SCAN_ROOTS: &[&str] = &["runtime", "sdk", "adapters"];

/// The one file holding the engine's node constructor.
const ALLOWED_FILE: &str = "runtime/streamlib-engine/src/iceoryx2/node.rs";

/// The signature of the one function in [`ALLOWED_FILE`] whose body may build a node.
const ALLOWED_CONSTRUCTOR_SIGNATURE: &str = "fn create_iceoryx2_node_in_domain(";

/// The call that builds a node, allowed only inside [`ALLOWED_CONSTRUCTOR_SIGNATURE`]'s body.
const NODE_BUILDER_CALL: &str = "NodeBuilder::new(";

/// The call that reads iceoryx2's lookup-path configuration, allowed nowhere.
const GLOBAL_CONFIG_CALL: &str = "Config::global_config(";

/// The builder call that opens a publish-subscribe service, allowed only in [`ALLOWED_FILE`].
const PUBLISH_SUBSCRIBE_BUILDER_CALL: &str = "publish_subscribe::<";

/// What a violating line built, and the fix the failure names for it.
#[derive(Debug, PartialEq, Eq)]
pub enum RefusedIceoryx2Construction {
    NodeOrGlobalConfigurationOutsideTheEngineOwnedDomain,
    PublishSubscribeServiceOutsideTheEngineWrapper,
}

#[derive(Debug)]
pub struct Violation {
    pub path: PathBuf,
    pub line_no: usize,
    pub line_text: String,
    pub refused_construction: RefusedIceoryx2Construction,
}

pub struct CheckReport {
    pub violations: Vec<Violation>,
    pub files_scanned: usize,
}

pub fn run(workspace_root: &Path) -> Result<()> {
    let report = scan(workspace_root)?;
    crate::ensure_source_walking_gate_read_source(
        "check-iceoryx2-construction",
        &format!("{SCAN_ROOTS:?}"),
        report.files_scanned,
        "an iceoryx2 node outside the engine-owned domain",
    )?;

    let failure_lines: Vec<String> = report
        .violations
        .iter()
        .map(|violation| {
            let refusal = match violation.refused_construction {
                RefusedIceoryx2Construction::NodeOrGlobalConfigurationOutsideTheEngineOwnedDomain => {
                    format!(
                        "an iceoryx2 node built outside `{ALLOWED_CONSTRUCTOR_SIGNATURE}..)` in \
                         {ALLOWED_FILE}, or any read of iceoryx2's global configuration, lands \
                         outside the engine-owned domain. Use \
                         `create_iceoryx2_node_in_engine_owned_domain` or `Iceoryx2Node::new` \
                         (in a test, `Iceoryx2Node::for_this_test_process`)."
                    )
                }
                RefusedIceoryx2Construction::PublishSubscribeServiceOutsideTheEngineWrapper => {
                    format!(
                        "a publish-subscribe service built outside {ALLOWED_FILE} lacks the \
                         sequence-number user header every channel data service carries. Open \
                         the channel through `Iceoryx2Node::open_or_create_service` and take its \
                         ports from the returned service."
                    )
                }
            };
            format!(
                "{}:{}: {refusal}\n    {}",
                violation.path.display(),
                violation.line_no,
                violation.line_text.trim(),
            )
        })
        .collect();
    anyhow::ensure!(
        failure_lines.is_empty(),
        "check-iceoryx2-construction found {} violation(s) across {} file(s):\n{}",
        failure_lines.len(),
        report.files_scanned,
        failure_lines.join("\n"),
    );

    tracing::info!(
        "check-iceoryx2-construction: {} files scanned across {SCAN_ROOTS:?}, no iceoryx2 \
         node built outside `{ALLOWED_CONSTRUCTOR_SIGNATURE}..)`, no publish-subscribe service \
         built outside {ALLOWED_FILE} and no global configuration read",
        report.files_scanned,
    );
    Ok(())
}

pub fn scan(workspace_root: &Path) -> Result<CheckReport> {
    let mut report = CheckReport {
        violations: Vec::new(),
        files_scanned: 0,
    };
    for scan_root in SCAN_ROOTS {
        for relative_path in crate::list_repository_files_under(workspace_root, scan_root)? {
            if !relative_path.ends_with(".rs") {
                continue;
            }
            let path = workspace_root.join(&relative_path);
            // `git ls-files --cached` lists a file deleted from the worktree but not
            // yet from the index.
            if !path.is_file() {
                continue;
            }
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            report.files_scanned += 1;
            collect_violations(Path::new(&relative_path), &content, &mut report.violations);
        }
    }
    Ok(report)
}

fn collect_violations(relative_path: &Path, content: &str, violations: &mut Vec<Violation>) {
    let is_the_allowed_file = relative_path == Path::new(ALLOWED_FILE);
    let allowed_constructor_body_lines = if is_the_allowed_file {
        line_indices_of_the_allowed_constructor_body(content)
    } else {
        None
    };
    for (index, line) in content.lines().enumerate() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        let inside_the_allowed_constructor = allowed_constructor_body_lines
            .as_ref()
            .is_some_and(|body_lines| body_lines.contains(&index));
        let builds_a_node_outside_the_allowed_constructor =
            !inside_the_allowed_constructor && line.contains(NODE_BUILDER_CALL);
        let refused_construction =
            if builds_a_node_outside_the_allowed_constructor || line.contains(GLOBAL_CONFIG_CALL) {
                RefusedIceoryx2Construction::NodeOrGlobalConfigurationOutsideTheEngineOwnedDomain
            } else if !is_the_allowed_file && line.contains(PUBLISH_SUBSCRIBE_BUILDER_CALL) {
                RefusedIceoryx2Construction::PublishSubscribeServiceOutsideTheEngineWrapper
            } else {
                continue;
            };
        violations.push(Violation {
            path: relative_path.to_path_buf(),
            line_no: index + 1,
            line_text: line.to_string(),
            refused_construction,
        });
    }
}

/// The zero-based line span from the allowed constructor's signature to its
/// closing brace, found by brace balance; `None` when the file no longer holds it,
/// so a moved constructor refuses every node builder rather than allowing any.
fn line_indices_of_the_allowed_constructor_body(
    content: &str,
) -> Option<std::ops::RangeInclusive<usize>> {
    let lines: Vec<&str> = content.lines().collect();
    let signature_index = lines
        .iter()
        .position(|line| line.contains(ALLOWED_CONSTRUCTOR_SIGNATURE))?;
    let mut brace_balance: i64 = 0;
    let mut body_opened = false;
    for (index, line) in lines.iter().enumerate().skip(signature_index) {
        for character in line.chars() {
            match character {
                '{' => {
                    brace_balance += 1;
                    body_opened = true;
                }
                '}' => brace_balance -= 1,
                _ => {}
            }
        }
        if body_opened && brace_balance == 0 {
            return Some(signature_index..=index);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn violations_in(content: &str) -> Vec<Violation> {
        let mut violations = Vec::new();
        collect_violations(
            Path::new("runtime/streamlib-engine/src/iceoryx2/input.rs"),
            content,
            &mut violations,
        );
        violations
    }

    #[test]
    fn a_raw_node_builder_in_library_code_is_refused() {
        let violations = violations_in(
            "fn open() {\n    let node = NodeBuilder::new().create::<ipc::Service>();\n}\n",
        );

        assert_eq!(violations.len(), 1, "{violations:?}");
        assert_eq!(violations[0].line_no, 2);
    }

    #[test]
    fn a_raw_node_builder_inside_a_unit_test_module_is_refused_too() {
        let violations = violations_in(
            "fn real() {}\n#[cfg(test)]\nmod tests {\n    fn t() { let node = NodeBuilder::new().create::<ipc::Service>().unwrap(); }\n}\n",
        );

        assert_eq!(violations.len(), 1, "{violations:?}");
    }

    #[test]
    fn a_read_of_the_global_iceoryx2_configuration_is_refused() {
        let violations = violations_in("let config = Config::global_config();\n");

        assert_eq!(violations.len(), 1, "{violations:?}");
    }

    #[test]
    fn a_raw_publish_subscribe_builder_inside_a_unit_test_module_is_refused() {
        let violations = violations_in(
            "fn real() {}\n#[cfg(test)]\nmod tests {\n    fn open() {\n        node.service_builder(&name)\n            .publish_subscribe::<[u8]>()\n            .open_or_create();\n    }\n}\n",
        );

        assert_eq!(violations.len(), 1, "{violations:?}");
        assert_eq!(violations[0].line_no, 6);
        assert_eq!(
            violations[0].refused_construction,
            RefusedIceoryx2Construction::PublishSubscribeServiceOutsideTheEngineWrapper
        );
    }

    #[test]
    fn a_channel_opened_through_the_engine_wrapper_passes() {
        let violations = violations_in(
            "let channel = node.open_or_create_service(&name, max_subscribers, depth)?;\nlet publisher = channel.create_publisher(expected_payload_bytes)?;\n// a raw `.publish_subscribe::<[u8]>()` carries no user header\n",
        );

        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn a_publish_subscribe_builder_anywhere_in_the_constructors_file_passes() {
        let violations = violations_in_the_allowed_file(&format!(
            "{ALLOWED_CONSTRUCTOR_SOURCE}\nfn open() {{\n    node.service_builder(&name).publish_subscribe::<[u8]>().open_or_create();\n}}\n"
        ));

        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn a_node_built_through_the_engine_domain_function_passes() {
        let violations = violations_in(
            "let node = Iceoryx2Node::new(&root, \"streamlib-runtime/R\")?;\nlet raw = create_iceoryx2_node_in_engine_owned_domain(&root, \"x\")?;\n",
        );

        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn a_commented_mention_is_not_a_call() {
        let violations = violations_in("// iceoryx2's NodeBuilder::new() reads the lookup path\n");

        assert!(violations.is_empty(), "{violations:?}");
    }

    const ALLOWED_CONSTRUCTOR_SOURCE: &str = "pub(crate) fn create_iceoryx2_node_in_domain(\n    root: &Path,\n) -> Result<Node<ipc::Service>> {\n    let name = NodeName::new(name).map_err(|refusal| {\n        Error::Configuration(format!(\"'{name}' is not a node name: {refusal:?}\"))\n    })?;\n    NodeBuilder::new()\n        .config(&config)\n        .create::<ipc::Service>()\n}\n";

    fn violations_in_the_allowed_file(content: &str) -> Vec<Violation> {
        let mut violations = Vec::new();
        collect_violations(Path::new(ALLOWED_FILE), content, &mut violations);
        violations
    }

    #[test]
    fn a_read_of_the_global_iceoryx2_configuration_is_refused_in_the_domain_function_file_too() {
        let violations = violations_in_the_allowed_file(&format!(
            "{ALLOWED_CONSTRUCTOR_SOURCE}fn config() {{\n    let mut config = Config::global_config().clone();\n}}\n"
        ));

        assert_eq!(violations.len(), 1, "{violations:?}");
        assert_eq!(violations[0].line_no, 12);
    }

    #[test]
    fn a_node_builder_inside_the_one_constructor_body_passes() {
        let violations = violations_in_the_allowed_file(ALLOWED_CONSTRUCTOR_SOURCE);

        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn a_node_builder_elsewhere_in_the_constructors_own_file_is_refused() {
        let violations = violations_in_the_allowed_file(&format!(
            "{ALLOWED_CONSTRUCTOR_SOURCE}\n#[cfg(test)]\nmod tests {{\n    fn helper() {{ let node = NodeBuilder::new().create::<ipc::Service>(); }}\n}}\n"
        ));

        assert_eq!(violations.len(), 1, "{violations:?}");
        assert_eq!(violations[0].line_no, 14);
    }

    #[test]
    fn a_node_builder_in_the_constructors_file_with_the_constructor_gone_is_refused() {
        let violations = violations_in_the_allowed_file(
            "pub fn renamed_constructor() {\n    NodeBuilder::new().create::<ipc::Service>()\n}\n",
        );

        assert_eq!(violations.len(), 1, "{violations:?}");
    }

    #[test]
    fn the_engine_domain_function_file_is_the_one_allowed_site() {
        let tmp = tempfile::TempDir::new().unwrap();
        let allowed = tmp.path().join(ALLOWED_FILE);
        std::fs::create_dir_all(allowed.parent().unwrap()).unwrap();
        std::fs::write(&allowed, ALLOWED_CONSTRUCTOR_SOURCE).unwrap();
        let bench = tmp.path().join("runtime/streamlib-engine/benches/hop.rs");
        std::fs::create_dir_all(bench.parent().unwrap()).unwrap();
        std::fs::write(
            &bench,
            "let node = NodeBuilder::new().create::<ipc::Service>();\nlet service = node.service_builder(&name).publish_subscribe::<[u8]>().create();\n",
        )
        .unwrap();
        std::process::Command::new("git")
            .arg("init")
            .arg("-q")
            .current_dir(tmp.path())
            .status()
            .unwrap();

        let report = scan(tmp.path()).unwrap();

        assert_eq!(report.files_scanned, 2);
        assert_eq!(report.violations.len(), 2, "{:?}", report.violations);
        assert!(
            report
                .violations
                .iter()
                .all(|violation| violation.path
                    == Path::new("runtime/streamlib-engine/benches/hop.rs")),
            "{:?}",
            report.violations
        );
    }
}

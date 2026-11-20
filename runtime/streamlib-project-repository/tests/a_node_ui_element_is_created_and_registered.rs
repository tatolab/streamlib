// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::path::PathBuf;

use serde_json::{Value, json};
use streamlib_node_schema::pin_type::BuiltInPinTypeName;
use streamlib_project_repository::{
    ProjectActionApplyOutcome, ProjectRepository, ProjectRepositoryRefusal,
    ProjectTransactionAuthor, ProjectTransactionAuthorKind,
};
use streamlib_runtime_client_contract::local_api_wire_contract::ProjectNodeUiElement;

const A_CONTROL_SCRIPT: &[u8] = b"export default class Knob extends HTMLElement {}\n";

struct AProject {
    _root: tempfile::TempDir,
    project_directory: PathBuf,
    repository: ProjectRepository,
}

impl AProject {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let project_directory = root.path().join("studio");
        let repository =
            ProjectRepository::create(&project_directory, "studio", &a_person()).unwrap();
        Self {
            _root: root,
            project_directory,
            repository,
        }
    }

    fn create(
        &mut self,
        element: &ProjectNodeUiElement,
        script: &str,
    ) -> Result<(), ProjectRepositoryRefusal> {
        self.repository
            .create_node_ui_element(&a_person(), None, element, script, A_CONTROL_SCRIPT)
            .map(|outcome| assert!(matches!(outcome, ProjectActionApplyOutcome::AppliedNow(_))))
    }

    fn on_disk(&self, path: &str) -> Option<Vec<u8>> {
        std::fs::read(self.project_directory.join(path)).ok()
    }

    fn manifest(&self) -> Value {
        serde_json::from_slice(&self.on_disk("node_ui/tatolab_node_ui.json").unwrap()).unwrap()
    }

    fn undo(&mut self) {
        let reversal = self.repository.undo_by(&a_person()).unwrap();
        self.repository
            .apply_project_operations(
                &a_person(),
                None,
                Vec::new(),
                reversal.operations,
                reversal.lineage,
            )
            .unwrap();
    }
}

fn a_person() -> ProjectTransactionAuthor {
    ProjectTransactionAuthor {
        kind: ProjectTransactionAuthorKind::Agent,
        name: "agent".to_owned(),
    }
}

fn a_float_knob() -> ProjectNodeUiElement {
    ProjectNodeUiElement::Control {
        pin_type: BuiltInPinTypeName::Float,
        control: Some("knob".to_owned()),
    }
}

#[test]
fn a_control_is_written_with_a_new_manifest_naming_it_and_one_undo_takes_both_out() {
    let mut project = AProject::new();

    project.create(&a_float_knob(), "knob.ts").unwrap();

    assert_eq!(
        project.on_disk("node_ui/knob.ts").unwrap(),
        A_CONTROL_SCRIPT
    );
    assert_eq!(
        project.manifest(),
        json!({
            "contract": "1.1",
            "controls": [{"type": "float", "control": "knob", "script": "knob.ts"}],
            "node_bodies": []
        })
    );

    project.undo();
    assert_eq!(project.on_disk("node_ui/knob.ts"), None);
    assert_eq!(project.on_disk("node_ui/tatolab_node_ui.json"), None);
}

#[test]
fn an_element_is_added_to_the_manifest_a_project_holds_and_one_undo_puts_it_back() {
    let mut project = AProject::new();
    project.create(&a_float_knob(), "knob.ts").unwrap();
    let manifest_before = project.on_disk("node_ui/tatolab_node_ui.json").unwrap();

    project
        .create(
            &ProjectNodeUiElement::NodeBody {
                node_type: "studio:Crossfade".to_owned(),
            },
            "bodies/crossfade.ts",
        )
        .unwrap();

    assert_eq!(
        project.manifest()["node_bodies"],
        json!([{"node_type": "studio:Crossfade", "script": "bodies/crossfade.ts"}])
    );
    assert_eq!(project.manifest()["controls"].as_array().unwrap().len(), 1);
    assert!(project.on_disk("node_ui/bodies/crossfade.ts").is_some());

    project.undo();
    assert_eq!(
        project.on_disk("node_ui/tatolab_node_ui.json").unwrap(),
        manifest_before
    );
    assert_eq!(project.on_disk("node_ui/bodies/crossfade.ts"), None);
}

#[test]
fn an_element_the_manifest_already_registers_or_a_script_already_there_is_refused() {
    let mut project = AProject::new();
    project.create(&a_float_knob(), "knob.ts").unwrap();

    assert!(matches!(
        project.create(&a_float_knob(), "another_knob.ts"),
        Err(ProjectRepositoryRefusal::NodeUiElementNotRegistered { path, why })
            if path == "node_ui/tatolab_node_ui.json" && why.contains("knob.ts")
    ));
    assert_eq!(project.on_disk("node_ui/another_knob.ts"), None);
    assert!(matches!(
        project.create(
            &ProjectNodeUiElement::Control {
                pin_type: BuiltInPinTypeName::Float,
                control: None,
            },
            "knob.ts",
        ),
        Err(ProjectRepositoryRefusal::ProjectFileAlreadyExists(path)) if path == "node_ui/knob.ts"
    ));
}

#[test]
fn a_manifest_that_does_not_read_takes_no_element_and_nothing_is_written() {
    let mut project = AProject::new();
    project
        .repository
        .write_project_file(
            &a_person(),
            None,
            "node_ui/tatolab_node_ui.json",
            br#"{"contract": "9.0"}"#,
            None,
        )
        .unwrap();

    assert!(matches!(
        project.create(&a_float_knob(), "knob.ts"),
        Err(ProjectRepositoryRefusal::NodeUiElementNotRegistered { why, .. })
            if why.contains("`contract`")
    ));
    assert_eq!(project.on_disk("node_ui/knob.ts"), None);
}

#[test]
fn a_script_outside_the_node_ui_folder_is_refused() {
    let mut project = AProject::new();

    assert!(matches!(
        project.create(&a_float_knob(), "../nodes/knob.ts"),
        Err(ProjectRepositoryRefusal::ProjectFilePathRefused { .. })
    ));
    assert_eq!(project.on_disk("node_ui/tatolab_node_ui.json"), None);
}

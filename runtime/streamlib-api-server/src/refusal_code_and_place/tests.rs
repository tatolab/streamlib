// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib::sdk::descriptors::{PinLinkEndNamed, PinLinkRefusal, PinType};
use streamlib::sdk::error::{Error, PortDirection, WhyANodeOfAStreamIsNotRunning};
use streamlib_project_repository::ProjectRepositoryRefusal;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ControlOperationRefusalCode, NodeNotRunningBecauseOnTheWire, ProjectActionRefusalOnTheWire,
    ProjectFilePlaceOnTheWire,
};

use super::RefusalCodeAndPlace;

/// `refusal` as a caller reads it back off the wire.
fn read_back(refusal: ProjectActionRefusalOnTheWire) -> ProjectActionRefusalOnTheWire {
    serde_json::from_value(refusal.the_refusal_as_json()).unwrap()
}

fn the_file(path: &str) -> Option<ProjectFilePlaceOnTheWire> {
    Some(ProjectFilePlaceOnTheWire {
        path: path.to_owned(),
        line: None,
        column: None,
    })
}

#[test]
fn a_project_file_the_project_does_not_hold_reads_back_as_file_not_found_at_its_path() {
    let refusal = ProjectRepositoryRefusal::NoSuchProjectFile("nodes/camera.py".to_owned());
    let read = read_back(
        RefusalCodeAndPlace::of_a_project_repository_refusal(&refusal)
            .on_the_refusal_of("read_project_file", refusal.to_string()),
    );
    assert_eq!(read.refusal_code, ControlOperationRefusalCode::FileNotFound);
    assert_eq!(read.refused_file, the_file("nodes/camera.py"));
    assert_eq!(read.action, "read_project_file");
    assert_eq!(read.runtime_words, refusal.to_string());
}

#[test]
fn a_write_over_a_file_changed_or_taken_out_since_its_read_reads_back_with_that_code() {
    for refusal in [
        ProjectRepositoryRefusal::ProjectFileChangedSinceItWasRead {
            path: "nodes/camera.py".to_owned(),
            sha256_read: "a".repeat(64),
            sha256_on_disk_now: "b".repeat(64),
        },
        ProjectRepositoryRefusal::ProjectFileRemovedSinceItWasRead("nodes/camera.py".to_owned()),
    ] {
        let read = read_back(
            RefusalCodeAndPlace::of_a_project_repository_refusal(&refusal)
                .on_the_refusal_of("write_project_file", refusal.to_string()),
        );
        assert_eq!(
            read.refusal_code,
            ControlOperationRefusalCode::FileChangedSinceItWasRead,
            "{refusal}"
        );
        assert_eq!(read.refused_file, the_file("nodes/camera.py"), "{refusal}");
    }
}

#[test]
fn a_project_repository_refusal_names_its_stream_node_or_file_and_its_kind() {
    let node = uuid::Uuid::new_v4();
    let cases = [
        (
            ProjectRepositoryRefusal::NoSuchStream(uuid::Uuid::new_v4()),
            ControlOperationRefusalCode::StreamNotFound,
            None,
            None,
        ),
        (
            ProjectRepositoryRefusal::ConfigReferencesNoAsset {
                node,
                asset: uuid::Uuid::new_v4(),
            },
            ControlOperationRefusalCode::Refused,
            Some(node.to_string()),
            None,
        ),
        (
            ProjectRepositoryRefusal::ProjectFilePathRefused {
                path: "../outside.py".to_owned(),
                why: "it leaves the project",
            },
            ControlOperationRefusalCode::Refused,
            None,
            the_file("../outside.py"),
        ),
        (
            ProjectRepositoryRefusal::ProjectFileUnreadable {
                path: "/rig/tatolab-project.json".into(),
                why: "expected value at line 1".to_owned(),
            },
            ControlOperationRefusalCode::Refused,
            None,
            the_file("/rig/tatolab-project.json"),
        ),
        (
            ProjectRepositoryRefusal::Io(std::io::Error::other("the disk is gone")),
            ControlOperationRefusalCode::RuntimeFailed,
            None,
            None,
        ),
        (
            ProjectRepositoryRefusal::BlankProjectName,
            ControlOperationRefusalCode::Refused,
            None,
            None,
        ),
    ];
    for (refusal, refusal_code, refused_node, refused_file) in cases {
        let read = read_back(
            RefusalCodeAndPlace::of_a_project_repository_refusal(&refusal)
                .on_the_refusal_of("transact", refusal.to_string()),
        );
        assert_eq!(read.refusal_code, refusal_code, "{refusal}");
        assert_eq!(read.refused_node, refused_node, "{refusal}");
        assert_eq!(read.refused_file, refused_file, "{refusal}");
    }
}

#[test]
fn an_engine_error_reads_back_with_its_code_and_the_input_it_names() {
    let cases = [
        (
            Error::NodePortNotFound {
                node_run_id: "blur".to_owned(),
                port_name: "radius".to_owned(),
                direction: PortDirection::Input,
            },
            ControlOperationRefusalCode::PortNotFound,
            Some("radius".to_owned()),
        ),
        (
            Error::NodePortNotFound {
                node_run_id: "blur".to_owned(),
                port_name: "video".to_owned(),
                direction: PortDirection::Output,
            },
            ControlOperationRefusalCode::PortNotFound,
            None,
        ),
        (
            Error::LinkAlreadyExists("camera.video -> blur.video".to_owned()),
            ControlOperationRefusalCode::LinkAlreadyExists,
            None,
        ),
        (
            Error::PinLinkRefused(Box::new(PinLinkRefusal::InputTakesNoWire {
                node_name: "blur".to_owned(),
                input_name: "radius".to_owned(),
            })),
            ControlOperationRefusalCode::InputTakesNoWire,
            Some("radius".to_owned()),
        ),
        (
            Error::PinLinkRefused(Box::new(PinLinkRefusal::PinTypesDoNotConnect {
                output: PinLinkEndNamed {
                    node_name: "camera".to_owned(),
                    port_name: "video".to_owned(),
                    member_name: None,
                    pin_type: PinType::Texture {
                        pixel_formats: Vec::new(),
                    },
                },
                input: PinLinkEndNamed {
                    node_name: "gain".to_owned(),
                    port_name: "level".to_owned(),
                    member_name: None,
                    pin_type: PinType::Float { exact_size: None },
                },
                conversions_that_bridge: Vec::new(),
            })),
            ControlOperationRefusalCode::LinkPinTypesDoNotConnect,
            None,
        ),
        (
            Error::NodeNameTaken {
                name: "blur".to_owned(),
                cast: "blur".to_owned(),
            },
            ControlOperationRefusalCode::NodeNameTaken,
            None,
        ),
        (
            Error::NodeNotFound("blur".to_owned()),
            ControlOperationRefusalCode::NodeNotFound,
            None,
        ),
        (
            Error::GpuError("the device was lost".to_owned()),
            ControlOperationRefusalCode::RuntimeFailed,
            None,
        ),
        (
            Error::Configuration("`radius` is not a number".to_owned()),
            ControlOperationRefusalCode::Refused,
            None,
        ),
        (
            Error::Runtime("the stream `live` is not started".to_owned()),
            ControlOperationRefusalCode::Refused,
            None,
        ),
        (
            Error::ShaderCompilation("`main` is not declared".to_owned()),
            ControlOperationRefusalCode::Refused,
            None,
        ),
        (
            Error::ClockError("the clock source went away".to_owned()),
            ControlOperationRefusalCode::RuntimeFailed,
            None,
        ),
    ];
    for (refusal, refusal_code, refused_field) in cases {
        let read = read_back(
            RefusalCodeAndPlace::of_an_engine_error(&refusal)
                .on_the_refusal_of("transact", refusal.to_string()),
        );
        assert_eq!(read.refusal_code, refusal_code, "{refusal}");
        assert_eq!(read.refused_field, refused_field, "{refusal}");
        assert_eq!(read.runtime_words, refusal.to_string());
    }
}

#[test]
fn a_node_of_a_stream_that_does_not_run_reads_back_as_node_not_running_saying_why() {
    let refusal = Error::NodeOfAStreamIsNotRunning {
        not_running_because: WhyANodeOfAStreamIsNotRunning::PlaybackPaused,
        runtime_words: "the project's playback is paused".to_owned(),
    };
    let read = read_back(
        RefusalCodeAndPlace::of_an_engine_error(&refusal)
            .on_the_refusal_of("open_viewer", refusal.to_string()),
    );
    assert_eq!(
        read.refusal_code,
        ControlOperationRefusalCode::NodeNotRunning
    );
    assert_eq!(
        read.not_running_because,
        Some(NodeNotRunningBecauseOnTheWire::PlaybackPaused)
    );
}

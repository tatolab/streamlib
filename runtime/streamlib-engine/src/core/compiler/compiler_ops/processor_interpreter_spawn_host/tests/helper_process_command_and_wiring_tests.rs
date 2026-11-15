// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The command, environment and working directory a helper starts with, and how
//! links are wired into it.

use super::*;

/// The helper a promoted function node runs in reports how it ran it in its
/// `ready`, and the report lands where the node's promotion holds it.
///
/// Fail-without-fix: the host's envelope knows no report for the node it
/// runs as its processor, so its `ready` is read and dropped.
#[test]
fn a_promoted_function_nodes_own_helper_reports_how_it_ran_it() {
    let import_path =
        crate::core::descriptors::ProcessorClassImportPath::new("my_app.ops:counter").unwrap();
    let mut node = ProcessorNode::new(import_path.clone(), None, "counter", None, vec![], vec![]);
    let how_its_own_helper_ran_it = Arc::new(HowItsHelpersRanAFunctionNode::default());
    node.insert_component_without_rendering_it(FunctionNodePromotedToItsOwnProcessComponent {
        process_execution: ProcessExecution::Continuous { interval_ms: 10 },
        how_its_own_helper_ran_it: Arc::clone(&how_its_own_helper_ran_it),
    });
    let host = spawn_host_for_a_function_node_promoted_to_its_own_process(
        "my_app.ops:counter",
        &ProcessorDescriptor::new(
            crate::core::descriptors::ProcessorClassShortName::new("counter").unwrap(),
            import_path,
            "a test double",
        ),
        &node,
        &stream_environment_for_test(),
        Path::new(LEND_DIRECTORY_FOR_TEST),
        the_stream_a_test_helper_belongs_to(),
    )
    .unwrap();

    host.link_wiring
        .note_how_its_far_side_ran_its_function_nodes(Some(&serde_json::json!({
            node.id.as_str(): false,
        })));

    assert_eq!(how_its_own_helper_ran_it.ran_compiled(), Some(false));
}

/// The child is told to run its loop in the mode the envelope carries, and
/// this host reports `Manual` for its own thread whatever the class
/// declared, so the envelope has to carry the child's own mode.
///
/// Fail-without-fix: build the envelope as `Reactive` regardless and a
/// `continuous` helper is told to run a reactive loop.
#[test]
fn the_wiring_envelope_carries_the_mode_the_child_drives_its_processor_in() {
    let import_path = crate::core::descriptors::ProcessorClassImportPath::new(
        "my_app.sinks:PollingSinkProcessor",
    )
    .unwrap();
    let descriptor = ProcessorDescriptor::new(
        crate::core::descriptors::ProcessorClassShortName::new("PollingSinkProcessor").unwrap(),
        import_path.clone(),
        "a test double",
    );
    let node = ProcessorNode::new(
        import_path,
        None,
        "PollingSinkProcessor",
        None,
        vec![],
        vec![],
    );

    for child_execution in [
        ProcessExecution::Continuous { interval_ms: 5 },
        ProcessExecution::Manual,
        ProcessExecution::Reactive,
    ] {
        let host = spawn_host_for_processor_node(
            "my_app.sinks:PollingSinkProcessor",
            &descriptor,
            ExecutionConfig::new(child_execution),
            &node,
            &stream_environment_for_test(),
            Path::new(LEND_DIRECTORY_FOR_TEST),
            the_stream_a_test_helper_belongs_to(),
        );
        assert_eq!(
            host.out_of_process_link_wiring()
                .expect("a helper host carries an envelope")
                .far_side_process_execution(),
            child_execution,
        );
    }
}

/// A link wired into a helper the host gave up on is refused, so the
/// compile wiring it fails and the caller hears it; before setup the same
/// link rides the setup command instead.
///
/// Fail-without-fix: set only `child_is_gone` when giving up and the
/// envelope, which the compiler reaches without this host's lock, goes on
/// recording links nothing will ever open.
#[test]
fn a_late_link_into_a_helper_the_host_gave_up_on_is_refused_and_one_before_setup_is_not() {
    let link_wiring = serde_json::json!({"link_id": "L-late", "name": "frames_from_upstream"});

    let mut failed = spawn_host_for_test();
    failed.give_up_on_the_helper_process();
    let refused = failed
        .out_of_process_link_wiring()
        .expect("a helper host carries an envelope")
        .record_a_link_and_hand_it_to_a_far_side_past_its_setup_command(
            crate::core::error::PortDirection::Input,
            link_wiring.clone(),
        )
        .expect_err("a dead child can open no port");
    assert!(
        refused.to_string().contains("has failed"),
        "the refusal names the failure; got {refused}"
    );

    let not_yet_set_up = spawn_host_for_test();
    let carried = not_yet_set_up
        .out_of_process_link_wiring()
        .expect("a helper host carries an envelope")
        .record_a_link_and_hand_it_to_a_far_side_past_its_setup_command(
            crate::core::error::PortDirection::Input,
            link_wiring,
        )
        .expect("before setup the envelope carries the link");
    assert!(
        carried.is_none(),
        "a link riding the setup command waits on no answer of its own"
    );
}

/// A link recorded while the helper sets up waits on its own answer, and a
/// setup that fails before its setup command goes out refuses that link
/// rather than leaving it pending.
///
/// Fail-without-fix: drop the hold and the link rides a setup command that
/// never goes out; drop the give-up and it reads pending for good.
#[test]
fn a_link_held_during_a_setup_that_fails_is_refused_rather_than_left_pending() {
    let mut host = spawn_host_for_test();
    let mut held_answer_cell = None;

    host.set_up_holding_every_later_link_until_the_setup_command_goes_out(|host| {
        held_answer_cell = host
            .out_of_process_link_wiring()
            .expect("a helper host carries an envelope")
            .record_a_link_and_hand_it_to_a_far_side_past_its_setup_command(
                crate::core::error::PortDirection::Input,
                serde_json::json!({"link_id": "L-live", "name": "frames_from_upstream"}),
            )
            .expect("a helper still setting up takes the link");
        Err(Error::Runtime(
            "the helper process could not be started".to_string(),
        ))
    })
    .expect_err("the setup failed");

    let held_answer_cell =
        held_answer_cell.expect("a link recorded during setup waits on its own answer");
    match held_answer_cell.the_far_sides_answer() {
        Some(crate::core::processors::OutOfProcessLinkWireOutcome::RefusedByTheFarSide {
            reason,
        }) => assert!(reason.contains("has failed"), "{reason}"),
        unanswered_or_opened => {
            panic!("the held link must be refused; got {unanswered_or_opened:?}")
        }
    }
}

/// The child is an exec of the stream's own venv interpreter running the
/// bootstrap the lend directory holds, by path — never `-m`, never `-c`,
/// never a fork, and never some other Python found on `PATH`.
#[test]
fn the_child_is_the_streams_interpreter_running_the_lent_bootstrap_by_path() {
    let command = spawn_host_for_test()
        .build_helper_process_command(
            "Rtest",
            "desk",
            Path::new("/tmp/streamlib-1000/iox2"),
            None,
            None,
            CaptureDeviceAccessPrompting::RefuseWhatWasNeverAskedFor,
        )
        .expect("the test directories hold no path-list separator");
    assert_eq!(
        command.get_program(),
        OsStr::new("/home/someone/my_app/.venv/bin/python")
    );
    let arguments: Vec<_> = command.get_args().collect();
    assert_eq!(
        arguments,
        [OsStr::new(
            "/opt/tatolab/lib/tatolab/lend/tatolab/runtime/_processor_interpreter_bootstrap.py"
        )]
    );
}

/// The bootstrap sits in the lent runtime package wherever the runtime
/// unit's layout puts that package.
#[test]
fn the_bootstrap_sits_in_the_lent_runtime_package_the_layout_declares() {
    let lent_runtime_package_prefix = format!(
        "{}/",
        streamlib_consumer_rhi::runtime_unit_layout::LENT_RUNTIME_PACKAGE_RELATIVE_TO_THE_LEND
    );
    let bootstrap_file_name = PROCESSOR_INTERPRETER_BOOTSTRAP_PATH_IN_THE_LEND_DIRECTORY
        .strip_prefix(&lent_runtime_package_prefix)
        .expect("the bootstrap path starts with the lent runtime package");
    assert!(!bootstrap_file_name.contains('/'));
}

/// The class the child imports, and the identifiers it reports itself by,
/// travel in the environment. `STREAMLIB_ENTRYPOINT` *is* the import path
/// the type was described under.
#[test]
fn the_child_is_told_which_class_to_import_and_who_it_is() {
    let spawn_host = spawn_host_for_test();
    let command = spawn_host
        .build_helper_process_command(
            "Rtest",
            "desk",
            Path::new("/tmp/streamlib-1000/iox2"),
            None,
            None,
            CaptureDeviceAccessPrompting::RefuseWhatWasNeverAskedFor,
        )
        .expect("the test directories hold no path-list separator");
    let environment = environment_of(&command);
    assert_eq!(
        value_of(&environment, "STREAMLIB_ENTRYPOINT"),
        Some("my_app.filters:BlurProcessor")
    );
    assert_eq!(
        value_of(&environment, "STREAMLIB_PROCESSOR_ID"),
        Some("Pblur")
    );
    assert_eq!(
        value_of(&environment, "STREAMLIB_RUNTIME_ID"),
        Some("Rtest")
    );
    assert_eq!(
        value_of(&environment, "STREAMLIB_NODE_NAME"),
        Some(spawn_host.processor_display_name.as_str())
    );
    assert_eq!(value_of(&environment, "STREAMLIB_STREAM_ID"), Some("desk"));
    assert_eq!(
        value_of(&environment, "STREAMLIB_STREAM_PROJECT_DIRECTORY"),
        spawn_host.stream_environment.project_directory.to_str()
    );
}

/// A device the helper's node opens prompts only as the stream's load
/// allows, so the child carries the stream's policy.
#[test]
fn the_child_is_handed_the_streams_capture_device_access_prompting() {
    for policy in [
        CaptureDeviceAccessPrompting::MayAskTheUser,
        CaptureDeviceAccessPrompting::RefuseWhatWasNeverAskedFor,
    ] {
        let command = spawn_host_for_test()
            .build_helper_process_command(
                "Rtest",
                "desk",
                Path::new("/tmp/streamlib-1000/iox2"),
                None,
                None,
                policy,
            )
            .expect("the test directories hold no path-list separator");
        let environment = environment_of(&command);
        assert_eq!(
            value_of(
                &environment,
                CAPTURE_DEVICE_ACCESS_PROMPTING_ENVIRONMENT_VARIABLE
            ),
            Some(policy.as_environment_value())
        );
    }
}

/// The child opens its iceoryx2 node in the domain its parent resolved, so
/// the two always share one domain whatever the child's working directory.
#[test]
fn the_child_is_handed_the_parents_iceoryx2_domain_root() {
    let command = spawn_host_for_test()
        .build_helper_process_command(
            "Rtest",
            "desk",
            Path::new("/tmp/streamlib-1000/iox2"),
            None,
            None,
            CaptureDeviceAccessPrompting::RefuseWhatWasNeverAskedFor,
        )
        .expect("the test directories hold no path-list separator");
    let environment = environment_of(&command);
    assert_eq!(
        value_of(&environment, "STREAMLIB_ICEORYX2_DOMAIN_ROOT"),
        Some("/tmp/streamlib-1000/iox2")
    );
}

/// The child is handed the id of the engine this parent was compiled from,
/// and refuses to start unless the engine it imports carries the same one.
#[test]
fn the_child_is_handed_the_engine_build_id_it_must_match() {
    let command = spawn_host_for_test()
        .build_helper_process_command(
            "Rtest",
            "desk",
            Path::new("/tmp/streamlib-1000/iox2"),
            None,
            None,
            CaptureDeviceAccessPrompting::RefuseWhatWasNeverAskedFor,
        )
        .expect("the test directories hold no path-list separator");
    let environment = environment_of(&command);
    assert_eq!(
        value_of(&environment, "STREAMLIB_ENGINE_BUILD_ID"),
        Some(ENGINE_BUILD_ID)
    );
}

/// The surface-share channel reaches the child under the variable its
/// exchange client reads — the socket path on Linux, the Mach service
/// name on macOS — and a helper started without one is told nothing.
#[test]
fn the_child_is_handed_the_surface_share_channel_under_its_variable() {
    let spawn_host = spawn_host_for_test();
    let command = spawn_host
        .build_helper_process_command(
            "Rtest",
            "desk",
            Path::new("/tmp/streamlib-1000/iox2"),
            Some(SurfaceShareChannelNamedToTheHelperProcess {
                environment_variable: "STREAMLIB_SURFACE_MACH_SERVICE",
                channel_name: OsStr::new("com.tatolab.streamlib.surface-share.Rtest"),
            }),
            None,
            CaptureDeviceAccessPrompting::RefuseWhatWasNeverAskedFor,
        )
        .expect("the test directories hold no path-list separator");
    assert_eq!(
        value_of(&environment_of(&command), "STREAMLIB_SURFACE_MACH_SERVICE"),
        Some("com.tatolab.streamlib.surface-share.Rtest")
    );

    let without_a_channel = spawn_host
        .build_helper_process_command(
            "Rtest",
            "desk",
            Path::new("/tmp/streamlib-1000/iox2"),
            None,
            None,
            CaptureDeviceAccessPrompting::RefuseWhatWasNeverAskedFor,
        )
        .expect("the test directories hold no path-list separator");
    let environment = environment_of(&without_a_channel);
    assert_eq!(
        value_of(&environment, "STREAMLIB_SURFACE_MACH_SERVICE"),
        None
    );
    assert_eq!(value_of(&environment, "STREAMLIB_SURFACE_SOCKET"), None);
}

/// The child is handed the device the runtime built on, spelled as the
/// wire spells a device UUID, and a helper started with no device is told
/// none.
#[test]
fn the_child_is_handed_the_vulkan_device_the_runtime_built_on() {
    let spawn_host = spawn_host_for_test();
    let runtime_vulkan_device_uuid = [
        0x94, 0x10, 0x5c, 0x9d, 0x1e, 0x3f, 0x69, 0x46, 0xc6, 0x65, 0x7e, 0x58, 0xe8, 0x20, 0x30,
        0x0e,
    ];
    let command = spawn_host
        .build_helper_process_command(
            "Rtest",
            "desk",
            Path::new("/tmp/streamlib-1000/iox2"),
            None,
            Some(runtime_vulkan_device_uuid),
            CaptureDeviceAccessPrompting::RefuseWhatWasNeverAskedFor,
        )
        .expect("the test directories hold no path-list separator");
    let handed_device_uuid = value_of(
        &environment_of(&command),
        "STREAMLIB_RUNTIME_VULKAN_DEVICE_UUID",
    )
    .map(str::to_owned);
    assert_eq!(
        handed_device_uuid.as_deref(),
        Some("94105c9d1e3f6946c6657e58e820300e")
    );
    assert_eq!(
        handed_device_uuid
            .as_deref()
            .and_then(streamlib_consumer_rhi::vulkan_device_uuid_from_hex),
        Some(runtime_vulkan_device_uuid)
    );

    let without_a_device = spawn_host
        .build_helper_process_command(
            "Rtest",
            "desk",
            Path::new("/tmp/streamlib-1000/iox2"),
            None,
            None,
            CaptureDeviceAccessPrompting::RefuseWhatWasNeverAskedFor,
        )
        .expect("the test directories hold no path-list separator");
    assert_eq!(
        value_of(
            &environment_of(&without_a_device),
            "STREAMLIB_RUNTIME_VULKAN_DEVICE_UUID"
        ),
        None
    );
}

/// The lend directory leads the child's `PYTHONPATH` so the venv borrows
/// `tatolab.runtime`, and the project directory follows so a processor
/// module sitting in the project imports — and nothing else is on it.
#[test]
fn the_childs_python_path_is_the_lend_directory_then_the_project_exactly() {
    let command = spawn_host_for_test()
        .build_helper_process_command(
            "Rtest",
            "desk",
            Path::new("/tmp/streamlib-1000/iox2"),
            None,
            None,
            CaptureDeviceAccessPrompting::RefuseWhatWasNeverAskedFor,
        )
        .expect("the test directories hold no path-list separator");
    assert_eq!(
        value_of(&environment_of(&command), "PYTHONPATH"),
        Some("/opt/tatolab/lib/tatolab/lend:/home/someone/my_app")
    );
}

/// Set only in the child process the inherited-`PYTHON*` test re-runs
/// itself in.
const INHERITED_PYTHON_VARIABLES_CHILD_ENVIRONMENT_VARIABLE: &str =
    "STREAMLIB_TEST_INHERITED_PYTHON_VARIABLES_CHILD";

/// The `PYTHON*` variables the re-run is started with, each of which would
/// change what a stream's interpreter imports or runs at startup.
const INHERITED_PYTHON_VARIABLES: [(&str, &str); 5] = [
    ("PYTHONHOME", "/inherited/home"),
    ("PYTHONSTARTUP", "/inherited/startup.py"),
    ("PYTHONSAFEPATH", "1"),
    ("PYTHONUSERBASE", "/inherited/user-base"),
    ("PYTHONPATH", "/inherited/by/the/runtime"),
];

/// Only a name beginning with `PYTHON` is removed; every other variable
/// the runtime carries, `STREAMLIB_*` included, is left to inherit.
#[test]
fn only_the_variables_whose_names_begin_with_python_are_removed() {
    let mut command = Command::new("/usr/bin/env");
    remove_every_python_variable_named_in(
        &mut command,
        [
            "PYTHONHOME",
            "PYTHONDONTWRITEBYTECODE",
            "STREAMLIB_RUNTIME_ID",
            "HOME",
            "MY_PYTHONPATH",
        ]
        .map(OsString::from),
    );
    let mut removed: Vec<_> = command
        .get_envs()
        .filter(|(_, value)| value.is_none())
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .collect();
    removed.sort();
    assert_eq!(removed, ["PYTHONDONTWRITEBYTECODE", "PYTHONHOME"]);
    assert_eq!(command.get_envs().count(), 2, "nothing else is touched");
}

/// Every `PYTHON*` variable the runtime was started with is kept from the
/// bootstrap and the describe, and `PYTHONPATH` is set outright to the lend
/// directory then the project; the compile starts under `-I`, which ignores
/// them all.
///
/// Asserted in a re-run of this test started with those variables,
/// because the process running the tests carries none to inherit.
#[test]
fn every_inherited_python_variable_is_kept_from_a_stream_interpreter_and_its_python_path_is_the_lend_then_the_project()
 {
    if std::env::var_os(INHERITED_PYTHON_VARIABLES_CHILD_ENVIRONMENT_VARIABLE).is_some() {
        assert_every_inherited_python_variable_is_kept_from_a_stream_interpreter();
        return;
    }

    let re_run = Command::new(std::env::current_exe().unwrap())
        .args([
            "core::compiler::compiler_ops::processor_interpreter_spawn_host::tests::helper_process_command_and_wiring_tests::\
             every_inherited_python_variable_is_kept_from_a_stream_interpreter_and_its_python_path_is_the_lend_then_the_project",
            "--exact",
            "--test-threads=1",
        ])
        .envs(INHERITED_PYTHON_VARIABLES)
        .env(INHERITED_PYTHON_VARIABLES_CHILD_ENVIRONMENT_VARIABLE, "1")
        .output()
        .expect("the test binary re-runs this test in a child process");
    let re_run_output = format!(
        "{}{}",
        String::from_utf8_lossy(&re_run.stdout),
        String::from_utf8_lossy(&re_run.stderr)
    );
    assert!(re_run.status.success(), "{re_run_output}");
    assert!(
        re_run_output.contains("1 passed"),
        "the re-run ran no test, so it asserted nothing: {re_run_output}"
    );
}

/// The re-run's half: the built commands, then a real child's environment.
fn assert_every_inherited_python_variable_is_kept_from_a_stream_interpreter() {
    for (name, value) in INHERITED_PYTHON_VARIABLES {
        assert_eq!(
            std::env::var(name).as_deref(),
            Ok(value),
            "the re-run was started with {name}"
        );
    }
    let bootstrap_command = spawn_host_for_test()
        .build_helper_process_command(
            "Rtest",
            "desk",
            Path::new("/tmp/streamlib-1000/iox2"),
            None,
            None,
            CaptureDeviceAccessPrompting::RefuseWhatWasNeverAskedFor,
        )
        .expect("the test directories hold no path-list separator");
    let describe_command =
        crate::core::compiler::compiler_ops::processor_interpreter_describe::processor_interpreter_describe_command(
            &[crate::core::descriptors::ProcessorClassImportPath::new(
                "my_app.filters:BlurProcessor",
            )
            .unwrap()],
            &stream_environment_for_test(),
            Path::new(LEND_DIRECTORY_FOR_TEST),
        )
        .expect("the test directories hold no path-list separator");
    for (command_kind, command) in [
        ("bootstrap", &bootstrap_command),
        ("describe", &describe_command),
    ] {
        for (name, _) in INHERITED_PYTHON_VARIABLES {
            let entry = command
                .get_envs()
                .find(|(entry_name, _)| *entry_name == OsStr::new(name))
                .map(|(_, value)| value.map(OsStr::to_os_string));
            let expected = if name == "PYTHONPATH" {
                Some(Some(OsString::from(
                    "/opt/tatolab/lib/tatolab/lend:/home/someone/my_app",
                )))
            } else {
                Some(None)
            };
            assert_eq!(entry, expected, "{name} on the {command_kind} command");
        }
    }

    let project_directory = tempfile::tempdir().expect("a project directory");
    let started = stream_interpreter_command_in_its_project_directory(
        Path::new("/usr/bin/env"),
        project_directory.path(),
        Path::new(LEND_DIRECTORY_FOR_TEST),
    )
    .expect("the test directories hold no path-list separator")
    .output()
    .expect("env runs");
    assert!(started.status.success());
    let environment_the_child_saw = String::from_utf8_lossy(&started.stdout).into_owned();
    let python_variables_the_child_saw: Vec<&str> = environment_the_child_saw
        .lines()
        .filter(|line| line.starts_with("PYTHON"))
        .collect();
    assert_eq!(
        python_variables_the_child_saw,
        [format!(
            "PYTHONPATH={LEND_DIRECTORY_FOR_TEST}:{}",
            project_directory.path().display()
        )],
        "{environment_the_child_saw}"
    );
    assert!(
        environment_the_child_saw
            .lines()
            .any(|line| line.starts_with(&format!(
                "{INHERITED_PYTHON_VARIABLES_CHILD_ENVIRONMENT_VARIABLE}="
            ))),
        "a variable not named PYTHON* is inherited: {environment_the_child_saw}"
    );
}

/// A directory holding the path-list separator would be split by Python
/// into entries naming other directories, so it is refused by name.
#[test]
fn a_project_directory_holding_the_path_list_separator_is_refused_by_name() {
    let refusal = processor_interpreter_bootstrap_command(
        &StreamEnvironment {
            project_directory: PathBuf::from("/home/someone/my:app"),
            interpreter: PathBuf::from("/home/someone/my:app/.venv/bin/python"),
        },
        Path::new(LEND_DIRECTORY_FOR_TEST),
    )
    .expect_err("a directory holding `:` cannot be on PYTHONPATH")
    .to_string();
    assert!(refusal.contains("`/home/someone/my:app`"), "{refusal}");
    assert!(refusal.contains("path-list separator"), "{refusal}");
}

/// The child runs in the project directory, so a relative path a node
/// opens resolves where the stream's author put it.
#[test]
fn the_childs_working_directory_is_the_project() {
    let command = spawn_host_for_test()
        .build_helper_process_command(
            "Rtest",
            "desk",
            Path::new("/tmp/streamlib-1000/iox2"),
            None,
            None,
            CaptureDeviceAccessPrompting::RefuseWhatWasNeverAskedFor,
        )
        .expect("the test directories hold no path-list separator");
    assert_eq!(
        command.get_current_dir(),
        Some(Path::new("/home/someone/my_app"))
    );
}

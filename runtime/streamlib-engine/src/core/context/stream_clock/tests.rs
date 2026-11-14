// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn a_real_clock_reads_the_machine_clock_itself() {
    let clock = StreamClock::starting_now(StreamClockSpec::Real);
    let before = machine_monotonic_now_ns();
    let stream_now = clock.now_ns();
    let after = machine_monotonic_now_ns();
    assert!(before <= stream_now && stream_now <= after);
    assert_eq!(clock.reading().mode, "real");
    assert_eq!(clock.reading().scale, 1.0);
}

#[test]
fn a_scaled_clock_starts_at_the_machine_clock_and_runs_at_its_scale() {
    let started_at = 1_000_000_000;
    let clock = StreamClock::starting_at(
        StreamClockSpec::Scaled {
            scale: 0.5,
            step_ns: DEFAULT_SCALED_STREAM_CLOCK_STEP_NS,
        },
        started_at,
    );
    assert_eq!(clock.stream_time_ns_at(started_at), started_at);
    assert_eq!(
        clock.stream_time_ns_at(started_at + 2_000_000),
        started_at + 1_000_000
    );
    assert_eq!(clock.reading().mode, "scaled");
    assert_eq!(clock.reading().scale, 0.5);
}

#[test]
fn a_published_clock_reads_the_same_in_any_process() {
    let started_at = 5_000;
    let clock = StreamClock::starting_at(
        StreamClockSpec::Scaled {
            scale: 0.25,
            step_ns: 10,
        },
        started_at,
    );
    let published = clock.published();
    assert_eq!(
        stream_time_ns_on_a_published_clock(&published, started_at + 4_000),
        clock.stream_time_ns_at(started_at + 4_000)
    );
    assert_eq!(
        stream_clock_reading_of_a_published_clock(&published),
        clock.reading()
    );
}

#[test]
fn a_stepped_clock_reads_its_open_steps_time() {
    let mut published = StreamClock::starting_at(
        StreamClockSpec::Stepped {
            step_ns: 100,
            pace: None,
            stop_after_steps: None,
        },
        7_000,
    )
    .published();
    published.step_index = 3;
    assert_eq!(
        stream_time_ns_on_a_published_clock(&published, i64::MAX),
        7_300
    );
}

#[test]
fn a_stepped_clock_reads_no_open_step_on_its_board_until_the_first_opens() {
    let stream_clock = StreamClock::starting_at(
        StreamClockSpec::Stepped {
            step_ns: 100,
            pace: None,
            stop_after_steps: None,
        },
        7_000,
    );
    assert_eq!(
        stream_clock_reading_of_a_published_clock(&stream_clock.published()).step_index,
        None
    );
    assert_eq!(stream_clock.open_the_waiting_step_at(7_000), Some(0));
    assert_eq!(
        stream_clock_reading_of_a_published_clock(&stream_clock.published()).step_index,
        Some(0)
    );
}

#[test]
fn the_spec_reads_and_writes_the_graph_documents_clock_key() {
    let read = |json: serde_json::Value| serde_json::from_value::<StreamClockSpec>(json);
    assert_eq!(
        read(serde_json::json!({"mode": "real"})).unwrap(),
        StreamClockSpec::Real
    );
    assert_eq!(
        read(serde_json::json!({"mode": "scaled", "scale": 0.5})).unwrap(),
        StreamClockSpec::Scaled {
            scale: 0.5,
            step_ns: DEFAULT_SCALED_STREAM_CLOCK_STEP_NS
        }
    );
    assert_eq!(
        read(serde_json::json!({"mode": "stepped", "step_ns": 10})).unwrap(),
        StreamClockSpec::Stepped {
            step_ns: 10,
            pace: None,
            stop_after_steps: None
        }
    );
    assert!(read(serde_json::json!({"mode": "scaled", "scale": 0.5, "speed": 2})).is_err());
    assert!(read(serde_json::json!({"mode": "warped"})).is_err());
    assert_eq!(
        serde_json::to_value(StreamClockSpec::Scaled {
            scale: 0.5,
            step_ns: 20
        })
        .unwrap(),
        serde_json::json!({"mode": "scaled", "scale": 0.5, "step_ns": 20})
    );
}

#[test]
fn a_spec_this_runtime_cannot_run_is_refused_naming_the_stream() {
    let refusal = |spec: StreamClockSpec| {
        spec.refusal_unless_this_runtime_runs_it("rig")
            .unwrap_err()
            .to_string()
    };
    assert!(
        refusal(StreamClockSpec::Scaled {
            scale: 0.0,
            step_ns: 1
        })
        .contains("`rig`")
    );
    assert!(
        refusal(StreamClockSpec::Scaled {
            scale: f64::NAN,
            step_ns: 1
        })
        .contains("finite")
    );
    assert!(
        refusal(StreamClockSpec::Stepped {
            step_ns: 0,
            pace: None,
            stop_after_steps: None
        })
        .contains("a step of 0 ns")
    );
    assert!(
        refusal(StreamClockSpec::Stepped {
            step_ns: 1,
            pace: Some(-1.0),
            stop_after_steps: None
        })
        .contains("paced at -1")
    );
    assert!(
        refusal(StreamClockSpec::Stepped {
            step_ns: 1,
            pace: None,
            stop_after_steps: Some(0)
        })
        .contains("after 0 steps")
    );
    StreamClockSpec::Stepped {
        step_ns: 1,
        pace: Some(2.0),
        stop_after_steps: Some(1),
    }
    .refusal_unless_this_runtime_runs_it("rig")
    .unwrap();
    StreamClockSpec::Real
        .refusal_unless_this_runtime_runs_it("rig")
        .unwrap();
    StreamClockSpec::Scaled {
        scale: 1.0,
        step_ns: 1,
    }
    .refusal_unless_this_runtime_runs_it("rig")
    .unwrap();
    StreamClockSpec::Scaled {
        scale: 8.0,
        step_ns: 1,
    }
    .refusal_unless_this_runtime_runs_it("rig")
    .unwrap();
}

fn a_half_speed_clock_started_at(machine_now_ns: i64) -> StreamClock {
    StreamClock::starting_at(
        StreamClockSpec::Scaled {
            scale: 0.5,
            step_ns: DEFAULT_SCALED_STREAM_CLOCK_STEP_NS,
        },
        machine_now_ns,
    )
}

#[test]
fn a_rescale_carries_stream_time_on_from_where_it_stands() {
    let clock = a_half_speed_clock_started_at(1_000);
    clock
        .rescale_at("warped", 0.25, 5_000)
        .expect("a scaled clock rescales");

    assert_eq!(clock.stream_time_ns_at(5_000), 3_000);
    assert_eq!(clock.stream_time_ns_at(9_000), 4_000);
    assert_eq!(clock.reading().scale, 0.25);
    assert_eq!(
        clock.spec(),
        StreamClockSpec::Scaled {
            scale: 0.25,
            step_ns: DEFAULT_SCALED_STREAM_CLOCK_STEP_NS
        }
    );
}

#[test]
fn a_rescale_is_refused_on_a_clock_that_does_not_scale_or_past_what_runs() {
    let real = StreamClock::starting_at(StreamClockSpec::Real, 0);
    let refusal = real.rescale_at("on-the-wall", 0.5, 10).unwrap_err();
    assert!(
        refusal
            .to_string()
            .contains("`on-the-wall` runs on a real clock")
    );

    let scaled = a_half_speed_clock_started_at(0);
    for refused_scale in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(scaled.rescale_at("warped", refused_scale, 10).is_err());
    }
    assert_eq!(scaled.reading().scale, 0.5);
    assert_eq!(scaled.stream_time_ns_at(10), 5);
}

#[test]
fn a_stream_deadline_converts_to_the_machine_time_the_clock_reaches_it() {
    let clock = a_half_speed_clock_started_at(1_000);
    assert_eq!(clock.machine_time_ns_reaching(1_500, 1_000), 2_000);
    clock
        .rescale_at("warped", 0.25, 2_000)
        .expect("a scaled clock rescales");
    assert_eq!(clock.machine_time_ns_reaching(1_750, 2_000), 3_000);

    let real = StreamClock::starting_at(StreamClockSpec::Real, 0);
    assert_eq!(real.machine_time_ns_reaching(700, 500), 700);
}

fn a_clock_at_four_times_real_time_started_at(machine_now_ns: i64) -> StreamClock {
    StreamClock::starting_at(
        StreamClockSpec::Scaled {
            scale: 4.0,
            step_ns: 100,
        },
        machine_now_ns,
    )
}

#[test]
fn a_scaled_clock_above_real_time_runs_in_steps_paced_at_its_scale() {
    let clock = a_clock_at_four_times_real_time_started_at(1_000);
    assert!(clock.runs_behind_the_step_barrier());
    assert_eq!(clock.stream_time_ns_at(9_000), 1_000);
    assert_eq!(clock.reading().step_ns, Some(100));
    assert_eq!(clock.reading().step_index, None);
    assert_eq!(clock.step_open_behind_the_barrier(), None);

    assert_eq!(clock.open_the_waiting_step_at(5_000), Some(0));
    assert_eq!(clock.open_the_waiting_step_at(5_000), None);
    assert_eq!(clock.stream_time_ns_at(9_000), 1_000);
    assert!(clock.open_the_step_after_at(0, 5_000));
    assert!(!clock.open_the_step_after_at(0, 5_000));
    assert_eq!(clock.stream_time_ns_at(9_000), 1_100);
    assert_eq!(clock.reading().step_index, Some(1));
    assert_eq!(clock.step_open_behind_the_barrier(), Some(1));
    assert_eq!(clock.machine_time_ns_the_step_opens(2), Some(5_050));
    assert_eq!(
        clock.wait_for_a_step_opened_after(Some(0), Duration::ZERO),
        Some(1)
    );
}

#[test]
fn crossing_real_time_live_moves_the_clock_in_and_out_of_steps_at_its_stream_time() {
    let clock = StreamClock::starting_at(
        StreamClockSpec::Scaled {
            scale: 0.5,
            step_ns: 100,
        },
        1_000,
    );
    assert!(!clock.runs_behind_the_step_barrier());
    assert_eq!(clock.machine_time_ns_the_step_opens(0), None);

    let reading = clock.rescale_at("warped", 4.0, 3_000).unwrap();
    assert_eq!(reading.step_ns, Some(100));
    assert!(clock.runs_behind_the_step_barrier());
    assert_eq!(clock.stream_time_ns_at(3_000), 2_000);
    assert_eq!(clock.stream_time_ns_at(90_000), 2_000);
    assert_eq!(clock.open_the_waiting_step_at(3_000), Some(0));
    assert!(clock.open_the_step_after_at(0, 3_000));
    assert_eq!(clock.stream_time_ns_at(3_000), 2_100);
    assert_eq!(clock.machine_time_ns_the_step_opens(2), Some(3_050));

    let reading = clock.rescale_at("warped", 0.5, 4_000).unwrap();
    assert_eq!((reading.step_ns, reading.step_index), (None, None));
    assert!(!clock.runs_behind_the_step_barrier());
    assert_eq!(clock.stream_time_ns_at(4_000), 2_100);
    assert_eq!(clock.stream_time_ns_at(6_000), 3_100);
    assert_eq!(clock.step_open_behind_the_barrier(), None);
    assert!(!clock.open_the_step_after_at(1, 6_000));
    assert_eq!(clock.open_the_waiting_step_at(6_000), None);
    assert_eq!(
        clock.wait_for_a_step_opened_after(Some(1), Duration::from_secs(5)),
        None
    );

    clock.rescale_at("warped", 2.0, 6_000).unwrap();
    assert_eq!(clock.stream_time_ns_at(6_000), 3_100);
    assert_eq!(clock.open_the_waiting_step_at(6_000), Some(2));
    assert_eq!(clock.stream_time_ns_at(6_000), 3_100);
    assert!(clock.open_the_step_after_at(2, 6_000));
    assert_eq!(clock.stream_time_ns_at(6_000), 3_200);
}

#[test]
fn a_rescale_above_real_time_keeps_the_open_step_and_paces_the_next_at_the_new_scale() {
    let clock = a_clock_at_four_times_real_time_started_at(0);
    assert_eq!(clock.open_the_waiting_step_at(0), Some(0));
    assert!(clock.open_the_step_after_at(0, 0));
    clock.rescale_at("warped", 2.0, 1_000).unwrap();
    assert_eq!(clock.step_open_behind_the_barrier(), Some(1));
    assert_eq!(clock.stream_time_ns_at(1_000), 100);
    assert_eq!(clock.machine_time_ns_the_step_opens(2), Some(1_050));
}

#[test]
fn a_paced_stepped_clocks_steps_open_no_earlier_than_their_share_of_real_time() {
    let stepped_at = |pace: Option<f64>| {
        StreamClock::starting_at(
            StreamClockSpec::Stepped {
                step_ns: 10_000_000,
                pace,
                stop_after_steps: None,
            },
            0,
        )
    };
    let paced_at_twice_real_time = stepped_at(Some(2.0));
    assert_eq!(
        paced_at_twice_real_time.open_the_waiting_step_at(1_000),
        Some(0)
    );
    assert_eq!(
        paced_at_twice_real_time.machine_time_ns_the_step_opens(4),
        Some(20_001_000)
    );
    let unpaced = stepped_at(None);
    assert_eq!(unpaced.open_the_waiting_step_at(1_000), Some(0));
    assert_eq!(unpaced.machine_time_ns_the_step_opens(4), None);
}

#[test]
fn a_scaled_clock_that_falls_behind_its_pace_slows_rather_than_bursting() {
    let clock = a_clock_at_four_times_real_time_started_at(0);
    assert_eq!(clock.open_the_waiting_step_at(0), Some(0));
    assert!(clock.open_the_step_after_at(0, 1_000));
    assert_eq!(clock.machine_time_ns_the_step_opens(2), Some(1_025));

    let stepped = StreamClock::starting_at(
        StreamClockSpec::Stepped {
            step_ns: 100,
            pace: Some(4.0),
            stop_after_steps: None,
        },
        0,
    );
    assert_eq!(stepped.open_the_waiting_step_at(0), Some(0));
    assert!(stepped.open_the_step_after_at(0, 1_000));
    assert_eq!(stepped.machine_time_ns_the_step_opens(2), Some(50));
}

#[test]
fn no_node_is_admitted_while_the_clock_steps() {
    let clock = StreamClock::starting_at(
        StreamClockSpec::Scaled {
            scale: 0.5,
            step_ns: 100,
        },
        0,
    );
    let mut admitted = false;
    assert!(clock.admit_unless_it_steps(|| admitted = true));
    assert!(admitted);
    assert!(
        clock
            .rescale_unless_a_node_holds_it_under_real_time("warped", 2.0, 10, || Some(
                "camera".to_owned()
            ))
            .unwrap_err()
            .to_string()
            .contains("`camera`")
    );
    clock.rescale_at("warped", 2.0, 10).unwrap();
    assert!(!clock.admit_unless_it_steps(|| unreachable!("refused while stepping")));
    assert!(clock.wait_until_it_runs_behind_the_step_barrier(Duration::ZERO));
}

#[test]
fn a_pause_holds_a_scaled_clock_where_it_stands_and_a_resume_carries_it_on_from_there() {
    let clock = a_half_speed_clock_started_at(1_000);
    clock.hold_where_it_stands_for_a_pause_at(3_000);
    assert!(clock.is_held_by_pause());
    assert_eq!(clock.stream_time_ns_at(3_000), 2_000);
    assert_eq!(clock.stream_time_ns_at(90_000), 2_000);
    assert_eq!(clock.published().scale_f64_bits, 0.0f64.to_bits());
    assert_eq!(clock.reading().scale, 0.5);

    clock.carry_on_from_where_the_pause_held_it_at(90_000);
    assert!(!clock.is_held_by_pause());
    assert_eq!(clock.stream_time_ns_at(90_000), 2_000);
    assert_eq!(clock.stream_time_ns_at(92_000), 3_000);
}

#[test]
fn a_rescale_while_paused_keeps_the_clock_held_and_the_resume_runs_at_the_new_scale() {
    let clock = a_half_speed_clock_started_at(0);
    clock.hold_where_it_stands_for_a_pause_at(2_000);
    let reading = clock.rescale_at("warped", 0.25, 5_000).unwrap();
    assert_eq!(reading.scale, 0.25);
    assert_eq!(clock.stream_time_ns_at(9_000), 1_000);

    clock.carry_on_from_where_the_pause_held_it_at(10_000);
    assert_eq!(clock.stream_time_ns_at(14_000), 2_000);
}

#[test]
fn a_paused_clock_behind_the_step_barrier_opens_no_step_and_paces_the_next_from_its_resume() {
    let clock = a_clock_at_four_times_real_time_started_at(0);
    assert_eq!(clock.open_the_waiting_step_at(0), Some(0));
    clock.hold_where_it_stands_for_a_pause_at(10);
    assert!(!clock.open_the_step_after_at(0, 1_000_000));
    assert_eq!(clock.step_open_behind_the_barrier(), Some(0));
    assert_eq!(clock.stream_time_ns_at(1_000_000), 0);

    clock.carry_on_from_where_the_pause_held_it_at(1_000_000);
    assert_eq!(clock.machine_time_ns_the_step_opens(1), Some(1_000_000));
    assert!(clock.open_the_step_after_at(0, 1_000_000));
    assert_eq!(clock.machine_time_ns_the_step_opens(2), Some(1_000_025));

    let stepped = StreamClock::starting_at(
        StreamClockSpec::Stepped {
            step_ns: 100,
            pace: Some(4.0),
            stop_after_steps: None,
        },
        0,
    );
    stepped.hold_where_it_stands_for_a_pause_at(0);
    assert_eq!(stepped.open_the_waiting_step_at(0), None);
    stepped.carry_on_from_where_the_pause_held_it_at(5_000);
    assert_eq!(stepped.open_the_waiting_step_at(5_000), Some(0));
    assert!(stepped.open_the_step_after_at(0, 5_000));
    assert_eq!(stepped.machine_time_ns_the_step_opens(2), Some(5_050));
}

#[test]
fn a_pause_leaves_a_real_time_clock_on_the_machine_clock() {
    let clock = StreamClock::starting_now(StreamClockSpec::Real);
    clock.hold_where_it_stands_for_a_pause();
    assert!(!clock.is_held_by_pause());
    let before = machine_monotonic_now_ns();
    let stream_now = clock.now_ns();
    assert!(before <= stream_now && stream_now <= machine_monotonic_now_ns());
}

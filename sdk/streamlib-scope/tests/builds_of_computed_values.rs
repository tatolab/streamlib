// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! How the builds of a scope's computed values stand, as a node's diagnostics read them: each
//! named by the type it holds and where it is declared, whether it holds a value, how its latest
//! build went, and nothing once the scope closes.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

mod common;

use common::Counted;
use streamlib_scope::{
    BuildFailure, BuildsOfAComputedValue, FrameView, LastBuild, ScopeBeingDeclared, computed,
};

/// A kernel built for a tap count, standing in for a compiled blur shader.
#[derive(Debug, PartialEq)]
struct BlurKernel {
    taps: u32,
}

const SETTLE: Duration = Duration::from_secs(5);

/// Run frames until `done` holds in one, failing once `SETTLE` runs out.
fn run_frames_until(view: &mut FrameView, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + SETTLE;
    while view.run_frame(&mut done) != Some(true) {
        assert!(std::time::Instant::now() < deadline, "no frame got there");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Mental-revert: a report read from the build's handle alone would name no computed value, and
/// one that read the view instead of the scope would show the failed first build as building.
#[test]
fn the_builds_of_each_computed_value_name_it_and_say_whether_it_holds_a_value() {
    let mut declared = ScopeBeingDeclared::new();
    let taps = declared.input(4_u32);
    let declared_on = line!() + 2;
    let kernel = declared.declare(|| {
        computed(move || {
            let taps = *taps.get();
            if taps.is_multiple_of(2) {
                return Err(format!("a blur needs an odd tap count, not {taps}"));
            }
            Ok(BlurKernel { taps })
        })
    });
    let (scope, mut view) = declared.start().unwrap();
    let builds = scope.builds();
    assert!(scope.wait_until_settled(SETTLE));

    let [failed_first] = builds.of_each_computed_value().try_into().unwrap();
    assert_eq!(failed_first.declared.at.file(), file!());
    assert_eq!(failed_first.declared.at.line(), declared_on);
    assert!(failed_first.declared.holding.ends_with("BlurKernel"));
    assert!(!failed_first.has_a_value);
    assert_eq!(
        failed_first.last_build,
        LastBuild::Failed {
            builds: 1,
            why: BuildFailure::Returned("a blur needs an odd tap count, not 4".to_string())
        }
    );

    scope.write(|writes| writes.write(taps, 5));
    run_frames_until(&mut view, || kernel.get().taps == 5);
    scope.write(|writes| writes.write(taps, 6));
    assert!(scope.wait_until_settled(SETTLE));
    let [failed_again] = builds.of_each_computed_value().try_into().unwrap();
    assert!(failed_again.has_a_value, "{failed_again:?}");
    assert!(matches!(
        failed_again.last_build,
        LastBuild::Failed {
            builds: 3,
            why: BuildFailure::Returned(_)
        }
    ));
}

/// Mental-revert: a reader that held the scope's slots would keep the built value alive past
/// the drop of its scope and view, to be dropped wherever the reader last went.
#[test]
fn the_builds_of_a_closed_scope_read_none_and_keep_no_value_alive() {
    let alive = Arc::new(AtomicUsize::new(0));
    let mut declared = ScopeBeingDeclared::new();
    let level = declared.input(3_u32);
    let counted = declared.declare(|| {
        let alive = Arc::clone(&alive);
        computed(move || Ok::<_, String>(Counted::new(*level.get(), &alive)))
    });
    let (scope, mut view) = declared.start().unwrap();
    let builds = scope.builds();
    run_frames_until(&mut view, || counted.get().value == 3);
    // The worker hands the value over before it reports it, under the lock settling waits on.
    assert!(scope.wait_until_settled(SETTLE));
    let built: Vec<BuildsOfAComputedValue> = builds.of_each_computed_value();
    assert_eq!(built.len(), 1);
    assert!(built[0].has_a_value);

    drop(scope);
    assert!(builds.of_each_computed_value().is_empty());
    drop(view);
    assert_eq!(
        alive.load(Ordering::SeqCst),
        0,
        "the reader keeps no built value alive"
    );
    assert!(builds.of_each_computed_value().is_empty());
}

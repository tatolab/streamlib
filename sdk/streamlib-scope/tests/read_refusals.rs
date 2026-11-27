// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `try_get` and the build failure a binding raises with it, for a binding that cannot let a
//! panic cross its caller's frames.

use std::time::Duration;

use streamlib_scope::{
    BuildFailure, LastBuild, ReadRefused, ScopeBeingDeclared, computed,
    fail_this_build_on_a_computed_value_not_yet_built,
};

const SETTLE: Duration = Duration::from_secs(5);

#[test]
fn try_get_answers_why_it_read_no_value_instead_of_panicking() {
    let mut other = ScopeBeingDeclared::new();
    let others_taps = other.input(3_u32);
    let mut declared = ScopeBeingDeclared::new();
    let taps = declared.input(3_u32);
    assert_eq!(
        taps.try_get().unwrap_err(),
        ReadRefused::OutsideABuildOrAFrame
    );
    assert_eq!(
        declared.declare(|| taps.try_get().unwrap_err()),
        ReadRefused::InTheBody
    );
    let (_scope, mut view) = declared.start().unwrap();
    assert_eq!(
        view.run_frame(|| others_taps.try_get().unwrap_err()),
        Some(ReadRefused::HandleOfAnotherScope)
    );
    assert_eq!(view.run_frame(|| *taps.try_get().unwrap()), Some(3));
}

#[test]
fn a_binding_that_saw_a_computed_value_not_yet_built_fails_its_build_the_same_way() {
    let mut declared = ScopeBeingDeclared::new();
    let taps = declared.input(4_u32);
    let (odd_taps, kernel_taps) = declared.declare(|| {
        let odd_taps = computed(move || {
            let taps = *taps.get();
            if taps.is_multiple_of(2) {
                return Err(format!("a blur needs an odd tap count, not {taps}"));
            }
            Ok(taps)
        });
        let kernel_taps = computed(move || {
            let odd_taps = match odd_taps.try_get() {
                Ok(odd_taps) => odd_taps,
                Err(refused) => {
                    assert_eq!(refused, ReadRefused::ComputedValueNotYetBuilt);
                    fail_this_build_on_a_computed_value_not_yet_built()
                }
            };
            Ok::<_, String>(*odd_taps)
        });
        (odd_taps, kernel_taps)
    });
    let (scope, mut view) = declared.start().unwrap();
    assert!(scope.wait_until_settled(SETTLE));
    assert!(matches!(
        scope.last_build_of(odd_taps),
        LastBuild::Failed { .. }
    ));
    assert_eq!(
        scope.last_build_of(kernel_taps),
        LastBuild::Failed {
            builds: 1,
            why: BuildFailure::ReadAComputedValueNotYetBuilt
        }
    );

    scope.write(|writes| writes.write(taps, 5));
    assert!(scope.wait_until_settled(SETTLE));
    assert_eq!(view.run_frame(|| *kernel_taps.get()), Some(5));
}

// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A loom model of a write, a build and a flush: in every interleaving, a frame sees both inputs
//! of one transaction or neither, a build reads both or neither, and closing the scope ends its
//! worker. Run with `RUSTFLAGS="--cfg loom"`.

#![cfg(loom)]

use streamlib_scope::{ScopeBeingDeclared, computed};

#[test]
fn a_transaction_is_never_seen_half_applied_and_closing_ends_the_worker() {
    let mut model = loom::model::Builder::new();
    model.preemption_bound = Some(3);
    model.check(|| {
        let mut declared = ScopeBeingDeclared::new();
        let first = declared.input(0_u32);
        let second = declared.input(0_u32);
        let pair =
            declared.declare(|| computed(move || Ok::<_, String>((*first.get(), *second.get()))));
        let (scope, mut view) = declared.start().unwrap();

        let writer = loom::thread::spawn(move || {
            scope.write(|writes| {
                writes.write(first, 1);
                writes.write(second, 1);
            });
            drop(scope);
        });
        for _ in 0..2 {
            view.run_frame(|| {
                assert_eq!(*first.get(), *second.get(), "a frame saw half a write");
                let (built_first, built_second) = *pair.get();
                assert_eq!(built_first, built_second, "a build saw half a write");
            });
        }
        writer.join().unwrap();

        // A frame after the close still sees both inputs of the write or neither.
        view.run_frame(|| {
            assert_eq!(*first.get(), *second.get(), "a frame saw half a write");
        });
    });
}

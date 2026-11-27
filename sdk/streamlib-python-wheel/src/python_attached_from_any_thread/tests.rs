// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::ffi::CString;

use pyo3::prelude::*;
use pyo3::types::PyDict;

use super::attached_from_any_thread;

#[test]
fn a_reference_dropped_while_attached_from_another_thread_is_released_on_that_thread() {
    Python::initialize();
    let (object, freed_on) = Python::attach(|python| {
        let globals = PyDict::new(python);
        let source = CString::new(
            "import threading\n\
             freed_on = []\n\
             class Recorded:\n    \
                 def __del__(self):\n        \
                     freed_on.append(threading.get_ident())\n\
             recorded = Recorded()\n\
             try:\n    \
                 recorded.close\n\
             except AttributeError as error:\n    \
                 raised = error\n",
        )
        .unwrap();
        python.run(&source, Some(&globals), None).unwrap();
        // The error holds the object as its `obj`, as a `getattr` of a missing method does.
        let raised = PyErr::from_value(globals.get_item("raised").unwrap().unwrap());
        globals.del_item("raised").unwrap();
        globals.del_item("recorded").unwrap();
        let freed_on = globals.get_item("freed_on").unwrap().unwrap().unbind();
        (raised, freed_on)
    });
    std::thread::spawn(move || {
        attached_from_any_thread(|python| {
            drop(object);
            assert_eq!(
                freed_on.bind(python).len().unwrap(),
                1,
                "the reference is released before the attach ends"
            );
        })
        .unwrap();
    })
    .join()
    .unwrap();
}

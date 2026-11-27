// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Which tables a module loads on and which it refuses, and the references
//! its handles hold on the node host.

use super::*;

#[test]
fn this_sdk_reads_table_1_13() {
    assert_eq!(
        (MODULE_HOST_TABLE_ABI_MAJOR, MODULE_HOST_TABLE_ABI_MINOR),
        (1, 13)
    );
}

#[test]
fn a_module_built_against_1_13_loads_on_a_runtime_lending_1_13_and_reaches_its_ports() {
    let node_host = stood_up_node_host();
    let mut fields_of_minor_13 = v1_13_table_over(node_host);
    fields_of_minor_13.abi_minor = 14;
    fields_of_minor_13.table_size_bytes = std::mem::size_of::<ModuleHostTableAtMinor14>() as u64;
    let table_at_minor_14 = ModuleHostTableAtMinor14 {
        fields_of_minor_13,
        a_verb_added_at_minor_14,
    };
    assert_eq!(
        unsafe { (table_at_minor_14.a_verb_added_at_minor_14)(std::ptr::null_mut()) },
        14
    );

    let module_host = unsafe {
        ModuleHost::from_module_host_table_pointer(
            std::ptr::from_ref(&table_at_minor_14).cast::<ModuleHostTable>(),
        )
    }
    .expect("a 1.13 module loads on a 1.14 runtime");

    assert_eq!(module_host.module_host_table_version(), (1, 14));
    let output_port = module_host
        .open_output_port("lent")
        .expect("the lent port opens");
    output_port.write(b"frame", Some(5), false).unwrap();
    output_port.write(b"stamped now", None, true).unwrap();
    assert_eq!(
        *node_host.writes.lock().unwrap(),
        vec![
            WriteTheHostRecorded {
                port: 7,
                payload: b"frame".to_vec(),
                timestamp_ns: 5,
                starts_group: false,
            },
            WriteTheHostRecorded {
                port: 7,
                payload: b"stamped now".to_vec(),
                timestamp_ns: MODULE_HOST_TABLE_TIMESTAMP_NOW,
                starts_group: true,
            },
        ]
    );
}

#[test]
fn a_runtime_of_another_major_is_refused_naming_both_versions() {
    let node_host = stood_up_node_host();
    let mut table = v1_13_table_over(node_host);
    table.abi_major = 2;
    table.abi_minor = 0;

    let refusal = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap_err();

    assert_eq!(
        refusal,
        ModuleHostError::ModuleHostTableMajorMismatch {
            runtime_major: 2,
            runtime_minor: 0,
            module_major: 1,
            module_minor: 13,
        }
    );
    let message = refusal.to_string();
    assert!(message.contains("module host table 1.13"), "{message}");
    assert!(message.contains("lends 2.0"), "{message}");
    assert_eq!(
        node_host.references.load(Ordering::SeqCst),
        0,
        "a refused table is never retained"
    );
}

#[test]
fn a_runtime_older_than_the_minor_a_module_needs_is_refused_naming_the_update() {
    let refusal =
        refuse_a_module_host_table_this_module_cannot_read(1, 0, 4096, 1, 1, 128).unwrap_err();

    assert_eq!(
        refusal.to_string(),
        "this module needs module host table 1.1, and this runtime lends 1.0 — update the runtime"
    );
}

#[test]
fn a_1_13_module_is_refused_on_a_runtime_lending_1_1() {
    let node_host = stood_up_node_host();
    let mut table = v1_13_table_over(node_host);
    table.abi_minor = 1;

    let refusal = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap_err();

    assert!(matches!(
        refusal,
        ModuleHostError::ModuleHostTableOlderThanTheModuleNeeds {
            runtime_minor: 1,
            module_minor: 13,
            ..
        }
    ));
}

#[test]
fn a_table_smaller_than_its_minor_states_is_refused() {
    let node_host = stood_up_node_host();
    let mut table = v1_13_table_over(node_host);
    table.table_size_bytes = 16;

    let refusal = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap_err();

    assert!(matches!(
        refusal,
        ModuleHostError::ModuleHostTableSmallerThanItsMinorStates {
            table_size_bytes: 16,
            ..
        }
    ));
}

#[test]
fn a_null_table_is_refused() {
    let refusal =
        unsafe { ModuleHost::from_module_host_table_pointer(std::ptr::null()) }.unwrap_err();
    assert_eq!(refusal, ModuleHostError::NoModuleHostTable);
}

#[test]
fn every_handle_on_the_node_host_holds_one_reference_until_it_drops() {
    let node_host = stood_up_node_host();
    let table = v1_13_table_over(node_host);

    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();
    let output_port = module_host.open_output_port("lent").unwrap();
    let clone = module_host.clone();
    assert_eq!(node_host.references.load(Ordering::SeqCst), 3);

    drop((module_host, output_port, clone));
    assert_eq!(node_host.references.load(Ordering::SeqCst), 0);
}

#[test]
fn a_module_built_against_1_13_is_refused_on_a_runtime_lending_1_0() {
    let node_host = stood_up_node_host();
    let mut table = v1_13_table_over(node_host);
    table.abi_minor = 0;

    let refusal = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap_err();

    assert_eq!(
        refusal.to_string(),
        "this module needs module host table 1.13, and this runtime lends 1.0 — update the runtime"
    );
}

/// A runtime lending 1.12 has no refusal of a module's own call in a frame,
/// so a module built against 1.13 never reads past the end of its table.
#[test]
fn a_module_built_against_1_13_is_refused_on_a_runtime_lending_1_12() {
    let node_host = stood_up_node_host();
    let mut table = v1_13_table_over(node_host);
    table.abi_minor = 12;

    let refusal = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap_err();

    assert_eq!(
        refusal.to_string(),
        "this module needs module host table 1.13, and this runtime lends 1.12 — update the runtime"
    );
}

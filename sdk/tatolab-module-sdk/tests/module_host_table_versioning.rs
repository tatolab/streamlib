// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What a module built against this SDK does with tables of other versions,
//! over a host stood up here in Rust.

use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use tatolab_module_sdk::module_host_table_abi::{
    MODULE_HOST_TABLE_STATUS_EMPTY, MODULE_HOST_TABLE_STATUS_OK, MODULE_HOST_TABLE_STATUS_REFUSED,
    MODULE_HOST_TABLE_STATUS_TIMED_OUT, MODULE_HOST_TABLE_TIMESTAMP_NOW, ModuleHostTableBytes,
    ModuleHostTableColorInfo, ModuleHostTableDmaBufImport, ModuleHostTableErrorMessage,
    ModuleHostTableNodeOutputTextureRequest, ModuleHostTablePortHandle, ModuleHostTableReadMessage,
    ModuleHostTableStatus, ModuleHostTableSurfaceHandle, ModuleHostTableUtf8,
};
use tatolab_module_sdk::{
    MODULE_HOST_TABLE_ABI_MAJOR, MODULE_HOST_TABLE_ABI_MINOR, ModuleHost, ModuleHostColorInfo,
    ModuleHostError, ModuleHostForeignDmaBuf, ModuleHostLogLevel,
    ModuleHostNodeOutputTextureRequest, ModuleHostTable,
    refuse_a_module_host_table_this_module_cannot_read,
};

/// One message a stood-up host was handed by a write.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WriteTheHostRecorded {
    port: u64,
    payload: Vec<u8>,
    timestamp_ns: i64,
    starts_group: bool,
}

/// The node host behind a stood-up table.
#[derive(Default)]
struct StoodUpNodeHost {
    references: AtomicUsize,
    writes: Mutex<Vec<WriteTheHostRecorded>>,
    log_records: Mutex<Vec<(u32, String, String)>>,
    released_reads: AtomicUsize,
    stop_requested: AtomicU32,
    surface_calls: Mutex<Vec<String>>,
}

fn node_host_of(node_host: *mut c_void) -> &'static StoodUpNodeHost {
    // SAFETY: every table here names a leaked `StoodUpNodeHost`.
    unsafe { &*node_host.cast::<StoodUpNodeHost>() }
}

unsafe extern "C" fn retain(node_host: *mut c_void) {
    node_host_of(node_host)
        .references
        .fetch_add(1, Ordering::SeqCst);
}

unsafe extern "C" fn release(node_host: *mut c_void) {
    node_host_of(node_host)
        .references
        .fetch_sub(1, Ordering::SeqCst);
}

unsafe extern "C" fn open_port_named_lent(
    _node_host: *mut c_void,
    port_name: ModuleHostTableUtf8,
    opened_port: *mut ModuleHostTablePortHandle,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    // SAFETY: the SDK passes a live view and live out-pointers.
    let port_name = unsafe { port_name.as_str() }.unwrap_or("");
    if port_name != "lent" {
        unsafe { (*error_message).write(&format!("port {port_name:?} was not lent")) };
        return MODULE_HOST_TABLE_STATUS_REFUSED;
    }
    unsafe { (*opened_port).value = 7 };
    MODULE_HOST_TABLE_STATUS_OK
}

unsafe extern "C" fn write(
    node_host: *mut c_void,
    port: ModuleHostTablePortHandle,
    payload: ModuleHostTableBytes,
    timestamp_ns: i64,
    starts_group: u32,
    _error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    node_host_of(node_host)
        .writes
        .lock()
        .unwrap()
        .push(WriteTheHostRecorded {
            port: port.value,
            // SAFETY: the SDK passes the payload it was handed.
            payload: unsafe { payload.as_slice() }.to_vec(),
            timestamp_ns,
            starts_group: starts_group != 0,
        });
    MODULE_HOST_TABLE_STATUS_OK
}

unsafe extern "C" fn refuse_every_catalog_entry(
    _node_host: *mut c_void,
    _port: ModuleHostTablePortHandle,
    _section: ModuleHostTableUtf8,
    _entry_json: ModuleHostTableUtf8,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    unsafe { (*error_message).write("the entry does not parse") };
    MODULE_HOST_TABLE_STATUS_REFUSED
}

static ONE_PAYLOAD_TO_READ: [u8; 3] = [1, 2, 3];

unsafe extern "C" fn read_one_message_then_empty(
    _node_host: *mut c_void,
    _port: ModuleHostTablePortHandle,
    read_message: *mut ModuleHostTableReadMessage,
    _error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    static READ_ALREADY: AtomicU32 = AtomicU32::new(0);
    if READ_ALREADY.swap(1, Ordering::SeqCst) == 1 {
        return MODULE_HOST_TABLE_STATUS_EMPTY;
    }
    unsafe {
        (*read_message).payload = ONE_PAYLOAD_TO_READ.as_ptr();
        (*read_message).payload_length = ONE_PAYLOAD_TO_READ.len();
        (*read_message).timestamp_ns = 42;
        (*read_message).starts_group = 1;
        (*read_message).lost_since_last_read = 3;
    }
    MODULE_HOST_TABLE_STATUS_OK
}

unsafe extern "C" fn count_a_released_read(
    node_host: *mut c_void,
    _read_message: *mut ModuleHostTableReadMessage,
) {
    node_host_of(node_host)
        .released_reads
        .fetch_add(1, Ordering::SeqCst);
}

unsafe extern "C" fn wait_times_out(
    _node_host: *mut c_void,
    _timeout_ns: u64,
    _error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    MODULE_HOST_TABLE_STATUS_TIMED_OUT
}

unsafe extern "C" fn stop_requested(node_host: *mut c_void) -> u32 {
    node_host_of(node_host)
        .stop_requested
        .load(Ordering::SeqCst)
}

unsafe extern "C" fn monotonic_now_ns(_node_host: *mut c_void) -> i64 {
    1_000
}

unsafe extern "C" fn log(
    node_host: *mut c_void,
    level: u32,
    target: ModuleHostTableUtf8,
    message: ModuleHostTableUtf8,
) {
    // SAFETY: the SDK passes live views.
    let (target, message) = unsafe { (target.as_str().unwrap(), message.as_str().unwrap()) };
    node_host_of(node_host).log_records.lock().unwrap().push((
        level,
        target.to_string(),
        message.to_string(),
    ));
}

unsafe extern "C" fn host_capabilities_json(_node_host: *mut c_void) -> ModuleHostTableUtf8 {
    ModuleHostTableUtf8::of("{\"platform\":\"stood-up\"}")
}

unsafe extern "C" fn import_dma_buf_as_surface_3(
    node_host: *mut c_void,
    dma_buf_import: *const ModuleHostTableDmaBufImport,
    imported_surface: *mut ModuleHostTableSurfaceHandle,
    _error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    // SAFETY: the SDK passes a live request and a live out-pointer.
    let dma_buf_import = unsafe { &*dma_buf_import };
    let pixel_format = unsafe { dma_buf_import.pixel_format.as_str() }.unwrap_or("");
    node_host_of(node_host)
        .surface_calls
        .lock()
        .unwrap()
        .push(format!(
            "import fd={} {}x{} {pixel_format} planes={} offsets={:?} strides={:?} size={}",
            dma_buf_import.fd,
            dma_buf_import.width,
            dma_buf_import.height,
            dma_buf_import.plane_count,
            &dma_buf_import.plane_offsets_bytes[..dma_buf_import.plane_count as usize],
            &dma_buf_import.plane_strides_bytes[..dma_buf_import.plane_count as usize],
            dma_buf_import.allocation_byte_size,
        ));
    unsafe { (*imported_surface).value = 3 };
    MODULE_HOST_TABLE_STATUS_OK
}

unsafe extern "C" fn acquire_output_texture_as_surface_4(
    node_host: *mut c_void,
    request: *const ModuleHostTableNodeOutputTextureRequest,
    acquired_surface: *mut ModuleHostTableSurfaceHandle,
    _error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    // SAFETY: as above.
    let request = unsafe { &*request };
    let (pool_key, texture_format, usage) = unsafe {
        (
            request.pool_key.as_str().unwrap_or(""),
            request.texture_format.as_str().unwrap_or(""),
            request.usage.as_str().unwrap_or(""),
        )
    };
    node_host_of(node_host)
        .surface_calls
        .lock()
        .unwrap()
        .push(format!(
            "acquire {pool_key} depth={} {}x{} {texture_format} {usage}",
            request.rotation_depth, request.width, request.height,
        ));
    unsafe { (*acquired_surface).value = 4 };
    MODULE_HOST_TABLE_STATUS_OK
}

unsafe extern "C" fn convert_refusing_an_unknown_range(
    node_host: *mut c_void,
    source: ModuleHostTableSurfaceHandle,
    destination: ModuleHostTableSurfaceHandle,
    source_color: *const ModuleHostTableColorInfo,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    // SAFETY: as above.
    let source_color = unsafe { &*source_color };
    let (matrix, range) = unsafe {
        (
            source_color.matrix.as_str().unwrap_or(""),
            source_color.range.as_str().unwrap_or(""),
        )
    };
    if range == "tv" {
        unsafe { (*error_message).write("\"tv\" is not a range name") };
        return MODULE_HOST_TABLE_STATUS_REFUSED;
    }
    node_host_of(node_host)
        .surface_calls
        .lock()
        .unwrap()
        .push(format!(
            "convert {} -> {} matrix={matrix:?} range={range:?}",
            source.value, destination.value,
        ));
    MODULE_HOST_TABLE_STATUS_OK
}

unsafe extern "C" fn surface_id_naming_the_handle(
    _node_host: *mut c_void,
    surface: ModuleHostTableSurfaceHandle,
) -> ModuleHostTableUtf8 {
    match surface.value {
        3 => ModuleHostTableUtf8::of("imported-3"),
        4 => ModuleHostTableUtf8::of("pool-slot#4"),
        _ => ModuleHostTableUtf8::of(""),
    }
}

unsafe extern "C" fn record_a_released_surface(
    node_host: *mut c_void,
    surface: ModuleHostTableSurfaceHandle,
) {
    node_host_of(node_host)
        .surface_calls
        .lock()
        .unwrap()
        .push(format!("release {}", surface.value));
}

unsafe extern "C" fn a_verb_added_at_minor_2(_node_host: *mut c_void) -> u32 {
    11
}

/// v1.1's fields, then one verb a later minor appended.
#[repr(C)]
struct ModuleHostTableAtMinor2 {
    fields_of_minor_1: ModuleHostTable,
    a_verb_added_at_minor_2: unsafe extern "C" fn(*mut c_void) -> u32,
}

fn stood_up_node_host() -> &'static StoodUpNodeHost {
    Box::leak(Box::new(StoodUpNodeHost::default()))
}

fn v1_1_table_over(node_host: &'static StoodUpNodeHost) -> ModuleHostTable {
    ModuleHostTable {
        abi_major: 1,
        abi_minor: 1,
        table_size_bytes: std::mem::size_of::<ModuleHostTable>() as u64,
        node_host: std::ptr::from_ref(node_host).cast_mut().cast(),
        node_host_retain: retain,
        node_host_release: release,
        output_port_open: open_port_named_lent,
        output_port_write: write,
        output_port_set_catalog_entry: refuse_every_catalog_entry,
        input_port_open: open_port_named_lent,
        input_port_read: read_one_message_then_empty,
        input_message_release: count_a_released_read,
        input_wait_for_any_port: wait_times_out,
        stop_requested,
        monotonic_now_ns,
        log,
        host_capabilities_json,
        surface_import_dma_buf: import_dma_buf_as_surface_3,
        output_texture_acquire_from_node_output_pool: acquire_output_texture_as_surface_4,
        surface_convert_into: convert_refusing_an_unknown_range,
        surface_id: surface_id_naming_the_handle,
        surface_release: record_a_released_surface,
    }
}

#[test]
fn this_sdk_reads_table_1_1() {
    assert_eq!(
        (MODULE_HOST_TABLE_ABI_MAJOR, MODULE_HOST_TABLE_ABI_MINOR),
        (1, 1)
    );
}

#[test]
fn a_module_built_against_1_1_loads_on_a_runtime_lending_1_2_and_reaches_its_ports() {
    let node_host = stood_up_node_host();
    let mut fields_of_minor_1 = v1_1_table_over(node_host);
    fields_of_minor_1.abi_minor = 2;
    fields_of_minor_1.table_size_bytes = std::mem::size_of::<ModuleHostTableAtMinor2>() as u64;
    let table_at_minor_2 = ModuleHostTableAtMinor2 {
        fields_of_minor_1,
        a_verb_added_at_minor_2,
    };
    assert_eq!(
        unsafe { (table_at_minor_2.a_verb_added_at_minor_2)(std::ptr::null_mut()) },
        11
    );

    let module_host = unsafe {
        ModuleHost::from_module_host_table_pointer(
            std::ptr::from_ref(&table_at_minor_2).cast::<ModuleHostTable>(),
        )
    }
    .expect("a 1.1 module loads on a 1.2 runtime");

    assert_eq!(module_host.module_host_table_version(), (1, 2));
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
    let mut table = v1_1_table_over(node_host);
    table.abi_major = 2;

    let refusal = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap_err();

    assert_eq!(
        refusal,
        ModuleHostError::ModuleHostTableMajorMismatch {
            runtime_major: 2,
            runtime_minor: 1,
            module_major: 1,
            module_minor: 1,
        }
    );
    let message = refusal.to_string();
    assert!(message.contains("module host table 1.1"), "{message}");
    assert!(message.contains("lends 2.1"), "{message}");
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
fn a_1_1_module_is_refused_on_a_runtime_lending_1_0() {
    let node_host = stood_up_node_host();
    let mut table = v1_1_table_over(node_host);
    table.abi_minor = 0;

    let refusal = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap_err();

    assert!(matches!(
        refusal,
        ModuleHostError::ModuleHostTableOlderThanTheModuleNeeds {
            runtime_minor: 0,
            module_minor: 1,
            ..
        }
    ));
}

#[test]
fn the_surface_verbs_carry_their_requests_and_a_surface_is_released_once_when_it_drops() {
    let node_host = stood_up_node_host();
    let table = v1_1_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let imported = module_host
        .import_dma_buf(&ModuleHostForeignDmaBuf {
            fd: 9,
            width: 64,
            height: 16,
            pixel_format: "nv12_full_range",
            allocation_byte_size: 3_456,
            plane_offsets_bytes: &[0, 2_304],
            plane_strides_bytes: &[128, 128],
            drm_format_modifier: 0,
        })
        .expect("the import is taken");
    let output_texture = module_host
        .acquire_texture_from_node_output_pool(&ModuleHostNodeOutputTextureRequest {
            pool_key: "video",
            rotation_depth: 3,
            width: 64,
            height: 16,
            texture_format: "rgba8_unorm",
            usage: &["storage_binding", "texture_binding"],
        })
        .expect("the acquire is taken");
    assert_eq!(imported.surface_id(), "imported-3");
    assert_eq!(output_texture.surface_id(), "pool-slot#4");

    module_host
        .convert_surface_into(
            &imported,
            &output_texture,
            &ModuleHostColorInfo {
                matrix: Some("bt709"),
                ..ModuleHostColorInfo::default()
            },
        )
        .expect("the conversion is taken");
    let refusal = module_host
        .convert_surface_into(
            &imported,
            &output_texture,
            &ModuleHostColorInfo {
                range: Some("tv"),
                ..ModuleHostColorInfo::default()
            },
        )
        .unwrap_err();
    assert_eq!(
        refusal.to_string(),
        "surface_convert_into: \"tv\" is not a range name"
    );
    drop((output_texture, imported));

    assert_eq!(
        *node_host.surface_calls.lock().unwrap(),
        vec![
            "import fd=9 64x16 nv12_full_range planes=2 offsets=[0, 2304] strides=[128, 128] \
             size=3456"
                .to_string(),
            "acquire video depth=3 64x16 rgba8_unorm storage_binding,texture_binding".to_string(),
            "convert 3 -> 4 matrix=\"bt709\" range=\"\"".to_string(),
            "release 4".to_string(),
            "release 3".to_string(),
        ]
    );
    assert_eq!(node_host.references.load(Ordering::SeqCst), 1);
}

#[test]
fn an_import_of_more_planes_than_the_table_carries_is_refused_before_the_host() {
    let node_host = stood_up_node_host();
    let table = v1_1_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let refusal = module_host
        .import_dma_buf(&ModuleHostForeignDmaBuf {
            fd: 9,
            width: 64,
            height: 16,
            pixel_format: "nv12_full_range",
            allocation_byte_size: 3_456,
            plane_offsets_bytes: &[0, 1, 2, 3, 4],
            plane_strides_bytes: &[128; 5],
            drm_format_modifier: 0,
        })
        .unwrap_err();
    assert!(refusal.to_string().contains("1 to 4 planes"), "{refusal}");
    assert!(node_host.surface_calls.lock().unwrap().is_empty());
}

#[test]
fn a_table_smaller_than_its_minor_states_is_refused() {
    let node_host = stood_up_node_host();
    let mut table = v1_1_table_over(node_host);
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
    let table = v1_1_table_over(node_host);

    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();
    let output_port = module_host.open_output_port("lent").unwrap();
    let clone = module_host.clone();
    assert_eq!(node_host.references.load(Ordering::SeqCst), 3);

    drop((module_host, output_port, clone));
    assert_eq!(node_host.references.load(Ordering::SeqCst), 0);
}

#[test]
fn a_refused_verb_carries_the_hosts_message_and_the_verb_it_refused() {
    let node_host = stood_up_node_host();
    let table = v1_1_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let refusal = module_host.open_output_port("never lent").unwrap_err();
    assert_eq!(
        refusal.to_string(),
        "output_port_open: port \"never lent\" was not lent"
    );

    let entry_refusal = module_host
        .open_output_port("lent")
        .unwrap()
        .set_catalog_entry("video", "{}")
        .unwrap_err();
    assert_eq!(
        entry_refusal.to_string(),
        "output_port_set_catalog_entry: the entry does not parse"
    );
}

#[test]
fn a_read_message_is_handed_back_to_the_host_when_it_drops() {
    let node_host = stood_up_node_host();
    let table = v1_1_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();
    let input_port = module_host.open_input_port("lent").unwrap();

    let message = input_port.read().unwrap().expect("one message waits");
    assert_eq!(message.payload(), &[1, 2, 3]);
    assert_eq!(message.timestamp_ns(), 42);
    assert!(message.starts_group());
    assert_eq!(message.lost_since_last_read(), 3);
    assert_eq!(node_host.released_reads.load(Ordering::SeqCst), 0);
    drop(message);
    assert_eq!(node_host.released_reads.load(Ordering::SeqCst), 1);

    assert!(input_port.read().unwrap().is_none());
}

#[test]
fn the_clock_stop_flag_wait_log_and_capabilities_reach_the_host() {
    let node_host = stood_up_node_host();
    let table = v1_1_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    assert_eq!(module_host.monotonic_now_ns(), 1_000);
    assert!(!module_host.stop_requested());
    node_host.stop_requested.store(1, Ordering::SeqCst);
    assert!(module_host.stop_requested());
    assert!(
        !module_host
            .wait_for_any_input_port(std::time::Duration::from_millis(1))
            .unwrap()
    );
    assert_eq!(
        module_host.host_capabilities_json(),
        "{\"platform\":\"stood-up\"}"
    );

    module_host.log(ModuleHostLogLevel::Warn, "my_module", "a record");
    assert_eq!(
        *node_host.log_records.lock().unwrap(),
        vec![(2, "my_module".to_string(), "a record".to_string())]
    );
}

#[cfg(feature = "tracing")]
#[test]
fn the_modules_own_tracing_records_reach_the_hosts_log_with_their_fields() {
    let node_host = stood_up_node_host();
    let table = v1_1_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    tatolab_module_sdk::module_host_tracing::forward_this_modules_tracing_records_to(&module_host);
    tracing::info!(target: "tatolab_webrtc", attempt = 2, "reconnecting");
    tatolab_module_sdk::module_host_tracing::stop_forwarding_this_modules_tracing_records();
    tracing::info!(target: "tatolab_webrtc", "not forwarded");

    assert_eq!(
        *node_host.log_records.lock().unwrap(),
        vec![(
            3,
            "tatolab_webrtc".to_string(),
            "reconnecting attempt=2".to_string()
        )]
    );
}

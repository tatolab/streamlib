// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

use super::*;
#[cfg(target_os = "linux")]
use tatolab_module_sdk::module_host_table_abi::ModuleHostTableBytes;

/// A parent stand-in that answers each request with `answer_for` and hands
/// back every request it saw once the channel closes.
fn parent_answering(
    mut parent_end: UnixStream,
    answer_for: fn(&serde_json::Value) -> serde_json::Value,
) -> std::thread::JoinHandle<Vec<serde_json::Value>> {
    std::thread::spawn(move || {
        let mut requests = Vec::new();
        loop {
            let mut length = [0u8; 4];
            if parent_end.read_exact(&mut length).is_err() {
                return requests;
            }
            let mut request = vec![0u8; u32::from_be_bytes(length) as usize];
            parent_end.read_exact(&mut request).expect("a request");
            let request: serde_json::Value =
                serde_json::from_slice(&request).expect("a JSON request");
            let mut answer = answer_for(&request);
            answer["request_id"] = request["request_id"].clone();
            requests.push(request);
            let answer = serde_json::to_vec(&answer).expect("an answer encodes");
            parent_end
                .write_all(&(answer.len() as u32).to_be_bytes())
                .expect("an answer length");
            parent_end.write_all(&answer).expect("an answer");
        }
    })
}

fn answer_like_the_engine(request: &serde_json::Value) -> serde_json::Value {
    match request["op"].as_str() {
        Some("acquire_texture") => serde_json::json!({
            "result": "ok",
            "handle_id": format!("pool-slot#{}", request["width"]),
        }),
        Some("convert_surface_into") if request["ycbcr_side_color"]["range"] == "tv" => {
            serde_json::json!({"result": "err", "message": "unknown range \"tv\""})
        }
        _ => serde_json::json!({"result": "ok"}),
    }
}

fn texture_request(width: u32) -> ModuleHostTableNodeOutputTextureRequest {
    ModuleHostTableNodeOutputTextureRequest {
        pool_key: ModuleHostTableUtf8::of("camera"),
        texture_format: ModuleHostTableUtf8::of("rgba8_unorm"),
        usage: ModuleHostTableUtf8::of("storage_binding, texture_binding"),
        width,
        height: 16,
        rotation_depth: 3,
        reserved: 0,
    }
}

fn color(range: &'static str) -> ModuleHostTableColorInfo {
    ModuleHostTableColorInfo {
        primaries: ModuleHostTableUtf8::of(""),
        transfer: ModuleHostTableUtf8::of(""),
        matrix: ModuleHostTableUtf8::of("bt709"),
        range: ModuleHostTableUtf8::of(range),
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_host_memory_write_over_an_imported_dma_buf_is_refused_before_any_staging_opens() {
    let surfaces = SurfacesLentToNativeCode::new(None, None);
    let imported = surfaces.lend(
        "foreign-dma-buf#1".to_string(),
        SurfaceLentToNativeCodeOrigin::AdoptedForeignSurface,
    );
    let pixels = [0u8; 16];
    let host_memory_pixels = ModuleHostTableHostMemoryPixels {
        pixels: ModuleHostTableBytes::of(&pixels),
        pixel_format: ModuleHostTableUtf8::of("bgra32"),
        width: 2,
        height: 2,
        row_stride_bytes: 8,
    };

    let refusal = surfaces
        .write_from_host_memory(imported, &host_memory_pixels)
        .unwrap_err();

    assert!(refusal.contains("imported foreign surface"), "{refusal}");
}

#[cfg(target_os = "linux")]
#[test]
fn a_host_memory_read_is_refused_by_name_before_it_reaches_the_parent() {
    let surfaces = SurfacesLentToNativeCode::new(None, None);
    let upstream_frame = surfaces.lend(
        "camera#4".to_string(),
        SurfaceLentToNativeCodeOrigin::NamedByAMessage,
    );
    let mut pixels = [0u8; 16];
    let to_fill =
        |pixel_format: &'static str, pixels: &mut [u8]| ModuleHostTableHostMemoryPixelsToFill {
            pixels: tatolab_module_sdk::module_host_table_abi::ModuleHostTableMutableBytes {
                bytes: pixels.as_mut_ptr(),
                length: pixels.len(),
            },
            pixel_format: ModuleHostTableUtf8::of(pixel_format),
            width: 2,
            height: 2,
            row_stride_bytes: 8,
        };

    let never_lent = surfaces
        .read_into_host_memory(
            ModuleHostTableSurfaceHandle { value: 99 },
            &to_fill("rgba32", &mut pixels),
        )
        .unwrap_err();
    let unknown_format = surfaces
        .read_into_host_memory(upstream_frame, &to_fill("not_a_format", &mut pixels))
        .unwrap_err();
    let no_channel = surfaces
        .read_into_host_memory(upstream_frame, &to_fill("rgba32", &mut pixels))
        .unwrap_err();

    assert!(never_lent.contains("handle 99"), "{never_lent}");
    assert!(unknown_format.contains("not_a_format"), "{unknown_format}");
    assert!(
        no_channel.contains("native escalate channel"),
        "{no_channel}"
    );
}

#[test]
fn a_conversion_reaches_the_parent_by_surface_id_and_its_source_is_released_once() {
    let (child_end, parent_end) = UnixStream::pair().expect("socketpair");
    let parent = parent_answering(parent_end, answer_like_the_engine);
    let surfaces = SurfacesLentToNativeCode::new(
        Some(Arc::new(HelperProcessNativeEscalateChannel::over(
            child_end,
        ))),
        None,
    );

    let source = surfaces
        .acquire_texture_from_node_output_pool(&texture_request(64))
        .expect("the source is acquired");
    let destination = surfaces
        .acquire_texture_from_node_output_pool(&texture_request(65))
        .expect("the destination is acquired");
    // SAFETY: the view lives until the handle is released.
    assert_eq!(
        unsafe { surfaces.surface_id_view(destination).as_str() },
        Ok("pool-slot#65")
    );
    surfaces
        .convert_surface_into(source, destination, &color("full"))
        .expect("the conversion is answered ok");
    let refusal = surfaces
        .convert_surface_into(source, destination, &color("tv"))
        .expect_err("the parent refuses");
    assert_eq!(refusal, "unknown range \"tv\"");
    surfaces.release(destination);
    surfaces.release(source);
    surfaces.release(source);
    drop(surfaces);

    let requests = parent.join().expect("the parent stand-in");
    let ops: Vec<&str> = requests
        .iter()
        .map(|request| request["op"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        ops,
        [
            "acquire_texture",
            "acquire_texture",
            "convert_surface_into",
            "convert_surface_into",
            "release_handle"
        ]
    );
    assert_eq!(
        requests[0]["usage"],
        serde_json::json!(["storage_binding", "texture_binding"])
    );
    assert_eq!(
        requests[0]["processor_output_pool"],
        serde_json::json!({"pool_key": "camera", "rotation_depth": 3})
    );
    assert_eq!(
        requests[2]["ycbcr_side_color"],
        serde_json::json!({"matrix": "bt709", "range": "full"})
    );
    assert_eq!(requests[2]["source_surface_id"], "pool-slot#64");
    assert_eq!(requests[4]["handle_id"], "pool-slot#64");
}

#[test]
fn a_lent_frame_is_never_released_at_the_parent_and_an_adopted_destination_is() {
    let (child_end, parent_end) = UnixStream::pair().expect("socketpair");
    let parent = parent_answering(parent_end, answer_like_the_engine);
    let surfaces = SurfacesLentToNativeCode::new(
        Some(Arc::new(HelperProcessNativeEscalateChannel::over(
            child_end,
        ))),
        None,
    );

    let upstream_frame = surfaces
        .lend_surface_a_message_names("upstream-pool-slot#2")
        .expect("the frame is lent");
    let adopted_nv12_surface = surfaces.lend(
        "adopted-iosurface".to_string(),
        SurfaceLentToNativeCodeOrigin::AdoptedForeignSurface,
    );
    surfaces
        .convert_surface_into(upstream_frame, adopted_nv12_surface, &color("limited"))
        .expect("the conversion is answered ok");
    surfaces.release(upstream_frame);
    surfaces.release(adopted_nv12_surface);
    assert!(
        surfaces.lend_surface_a_message_names("").is_err(),
        "an empty id names nothing"
    );
    drop(surfaces);

    let requests = parent.join().expect("the parent stand-in");
    let ops: Vec<&str> = requests
        .iter()
        .map(|request| request["op"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(ops, ["convert_surface_into", "release_handle"]);
    assert_eq!(requests[0]["source_surface_id"], "upstream-pool-slot#2");
    assert_eq!(requests[1]["handle_id"], "adopted-iosurface");
}

#[test]
fn a_handle_the_table_never_lent_is_refused_by_value() {
    let surfaces = SurfacesLentToNativeCode::new(None, None);
    let refusal = surfaces
        .convert_surface_into(
            ModuleHostTableSurfaceHandle { value: 7 },
            ModuleHostTableSurfaceHandle { value: 8 },
            &color(""),
        )
        .expect_err("no surface was lent");
    assert!(refusal.contains("handle 7"), "{refusal}");
    // SAFETY: an empty view.
    assert_eq!(
        unsafe {
            surfaces
                .surface_id_view(ModuleHostTableSurfaceHandle { value: 7 })
                .as_str()
        },
        Ok("")
    );
}

#[test]
fn a_helper_without_a_native_channel_refuses_naming_it() {
    let surfaces = SurfacesLentToNativeCode::new(None, None);
    let refusal = surfaces
        .acquire_texture_from_node_output_pool(&texture_request(64))
        .expect_err("no channel");
    assert!(refusal.contains("native escalate channel"), "{refusal}");
}

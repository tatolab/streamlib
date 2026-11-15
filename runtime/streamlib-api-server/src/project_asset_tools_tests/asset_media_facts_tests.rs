// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The media facts an imported asset is read for once, announced on the feed
//! and described with, and the stream asset that carries none.

use super::*;

/// Every `media_facts_read` entry on the project's change feed, by asset UUID.
async fn the_assets_whose_media_facts_the_feed_says_were_read(
    listener: &RunningService<RoleClient, ()>,
    transactions_resource_uri: &str,
) -> Vec<String> {
    let read = listener
        .read_resource(ReadResourceRequestParams::new(
            transactions_resource_uri.to_owned(),
        ))
        .await
        .unwrap();
    let read = serde_json::to_value(read).unwrap();
    let feed: Value = serde_json::from_str(read["contents"][0]["text"].as_str().unwrap()).unwrap();
    feed["transactions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["status"] == "media_facts_read")
        .map(|entry| entry["asset"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn an_imported_image_lut_and_fmu_are_read_once_announced_and_described_with_their_facts() {
    let served = AssetToolsServedOverAProject::over_a_new_project().await;
    let transactions_resource_uri =
        served.answered("list_assets", json!({})).await["transactions_resource_uri"]
            .as_str()
            .unwrap()
            .to_owned();
    let listener =
        crate::mcp_stdio_upgrade::tests::rmcp_client_over_the_upgraded_stream(&served.served).await;
    let mut subscription = listener
        .listen(
            rmcp::model::SubscriptionFilter::builder()
                .resource_subscription(transactions_resource_uri.clone())
                .build(),
        )
        .await
        .unwrap();

    let cover = served.project_parent_directory.path().join("cover.png");
    image::RgbImage::from_pixel(1024, 512, image::Rgb([10, 120, 200]))
        .save(&cover)
        .unwrap();
    let grade = served
        .a_file_outside_the_project("grade.cube", b"TITLE \"grade\"\nLUT_3D_SIZE 33\n0 0 0\n");
    let arm = served.a_file_outside_the_project("arm.fmu", b"an FMU");
    let mut imported_assets = Vec::new();
    for source in [&cover, &grade, &arm] {
        let imported = served
            .answered("import_asset", json!({ "source": source }))
            .await;
        imported_assets.push(imported["asset"]["asset"].as_str().unwrap().to_owned());
    }
    let [cover_asset, grade_asset, arm_asset] = imported_assets.clone().try_into().unwrap();

    loop {
        let read_assets = the_assets_whose_media_facts_the_feed_says_were_read(
            &listener,
            &transactions_resource_uri,
        )
        .await;
        if imported_assets
            .iter()
            .all(|asset| read_assets.contains(asset))
        {
            break;
        }
        tokio::time::timeout(std::time::Duration::from_secs(10), subscription.next())
            .await
            .expect("each read is announced on the change feed")
            .unwrap()
            .expect("the listen is still open");
    }

    let cover_described = served
        .answered("describe_asset", json!({ "asset": cover_asset }))
        .await;
    let cover_described = &cover_described["asset"];
    assert_eq!(
        cover_described["media_facts_state"], "read",
        "{cover_described}"
    );
    assert_eq!(
        cover_described["media_facts"]["image"],
        json!({ "pixel_width": 1024, "pixel_height": 512 })
    );
    assert_eq!(
        cover_described["preview"],
        json!({ "pixel_width": 512, "pixel_height": 256, "media_type": "image/png" })
    );
    let preview = served
        .answered("read_asset_preview", json!({ "asset": cover_asset }))
        .await;
    assert_eq!(preview["media_type"], "image/png");
    let png_bytes = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        preview["content_base64"].as_str().unwrap(),
    )
    .unwrap();
    assert_eq!(preview["byte_len"], png_bytes.len());
    let decoded = image::load_from_memory(&png_bytes).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (512, 256));
    let content_sha256 = cover_described["content_sha256"].as_str().unwrap();
    assert!(exists_under(
        &served.project_directory(),
        &format!(".tatolab/asset-media/{content_sha256}/png/preview.png")
    ));

    let grade_described = served
        .answered("describe_asset", json!({ "asset": grade_asset }))
        .await;
    assert_eq!(
        grade_described["asset"]["media_facts"]["lut"]["lut_3d_size"], 33,
        "{grade_described}"
    );
    assert_eq!(grade_described["asset"]["preview"], Value::Null);
    let refused = served
        .tool_call("read_asset_preview", json!({ "asset": grade_asset }))
        .await;
    assert!(
        the_error_text_of(&refused).contains("has no preview picture"),
        "{refused}"
    );

    let arm_described = served
        .answered("describe_asset", json!({ "asset": arm_asset }))
        .await;
    assert_eq!(arm_described["asset"]["media_facts_state"], "unreadable");
    assert_eq!(
        arm_described["asset"]["media_facts_unreadable_because"],
        "the runtime has no reader for `.fmu` files"
    );
    assert_eq!(arm_described["asset"]["media_facts"], Value::Null);

    let listed = served
        .answered("list_assets", json!({ "kind": "media" }))
        .await;
    assert_eq!(listed["assets"][0]["asset"], cover_asset.as_str());
    assert_eq!(listed["assets"][0]["media_facts_state"], "read", "{listed}");
    assert_eq!(
        listed["assets"][0]["media_facts"]["image"]["pixel_width"],
        1024
    );
}

#[tokio::test]
async fn a_stream_asset_carries_no_media_facts_and_has_no_preview() {
    let served = AssetToolsServedOverAProject::over_a_new_project().await;
    served
        .answered(
            "create_stream",
            json!({ "path": "streams/main.graph.json", "display_name": "Main" }),
        )
        .await;
    let listed = served
        .answered("list_assets", json!({ "kind": "stream" }))
        .await;
    let main = &listed["assets"][0];
    assert_eq!(main["kind"], "stream", "{listed}");
    assert_eq!(main["media_facts_state"], Value::Null);
    let refused = served
        .tool_call("read_asset_preview", json!({ "asset": main["asset"] }))
        .await;
    assert!(
        the_error_text_of(&refused).contains("is a stream, which has no preview"),
        "{refused}"
    );
}

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Microbench for issue #1407: the per-frame cost of reading a payload as a
//! dynamic [`Bag`] versus deserializing it into a codegen struct.
//!
//! Both arms start from the identical `to_vec_named` msgpack bytes a producer
//! puts on the wire. The delta is what a processor pays to trade a generated
//! type for schema-free field access:
//!
//! - `codegen_deserialize` — `rmp_serde::from_slice::<Frame>` straight into a
//!   typed struct (today's path when a schema package exists).
//! - `bag_eager_decode` — `Bag::from_msgpack` (eager decode into the value
//!   tree) plus two typed `get::<T>` reads, the schema-free path.
//! - `bag_encode` — `Bag::to_msgpack`, the write-side counterpart, versus
//!   `to_vec_named` of the typed struct.
//!
//! Run: `cargo bench -p streamlib-plugin-sdk --bench bag_decode`.

use criterion::{Criterion, criterion_group, criterion_main};
use serde::{Deserialize, Serialize};
use std::hint::black_box;
use streamlib_plugin_sdk::sdk::bag::Bag;

#[derive(Serialize, Deserialize)]
struct Frame {
    width: u32,
    height: u32,
    pts_ns: i64,
    codec: String,
    keyframe: bool,
}

fn sample_frame() -> Frame {
    Frame {
        width: 1920,
        height: 1080,
        pts_ns: 1_234_567_890,
        codec: "h265".to_owned(),
        keyframe: true,
    }
}

fn bench_bag(c: &mut Criterion) {
    let bytes = rmp_serde::to_vec_named(&sample_frame()).unwrap();

    c.bench_function("codegen_deserialize", |b| {
        b.iter(|| {
            let frame: Frame = rmp_serde::from_slice(black_box(&bytes)).unwrap();
            black_box((frame.width, frame.keyframe));
        });
    });

    c.bench_function("bag_eager_decode", |b| {
        b.iter(|| {
            let bag = Bag::from_msgpack(black_box(&bytes)).unwrap();
            let width: u32 = bag.get("width").unwrap();
            let keyframe: bool = bag.get("keyframe").unwrap();
            black_box((width, keyframe));
        });
    });

    let mut authored = Bag::new();
    authored.set("width", 1920_u32).unwrap();
    authored.set("height", 1080_u32).unwrap();
    authored.set("pts_ns", 1_234_567_890_i64).unwrap();
    authored.set("codec", "h265").unwrap();
    authored.set("keyframe", true).unwrap();

    c.bench_function("bag_encode", |b| {
        b.iter(|| {
            let out = black_box(&authored).to_msgpack().unwrap();
            black_box(out);
        });
    });

    c.bench_function("codegen_serialize", |b| {
        let frame = sample_frame();
        b.iter(|| {
            let out = rmp_serde::to_vec_named(black_box(&frame)).unwrap();
            black_box(out);
        });
    });
}

criterion_group!(benches, bench_bag);
criterion_main!(benches);

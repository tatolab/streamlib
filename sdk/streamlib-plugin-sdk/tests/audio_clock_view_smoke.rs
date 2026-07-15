// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! chord_generator-shaped compile smoke: an audio package authored
//! against the engine-free SDK's public surface reaches the host's audio
//! clock through the runtime-context view's `audio_clock()` accessor and
//! registers an `on_tick` callback typed on the engine-free
//! `AudioTickContext`. This file compiles against
//! `streamlib_plugin_sdk::sdk::context::*` exactly as an external package
//! does — it proves the public paths resolve and the accessor + shim +
//! tick-context types compose end-to-end.

use streamlib_plugin_sdk::sdk::context::{
    AudioClockShim, AudioTickContext, RuntimeContextFullAccess, RuntimeContextLimitedAccess,
};

/// Shape of a chord generator's `setup()` body: obtain the audio clock
/// from the privileged runtime-context view, read its cadence, and
/// register a tick callback. Compile-only — never invoked (there is no
/// live host in a unit-test build), but it must type-check against the
/// public SDK surface.
#[allow(dead_code)]
fn chord_generator_setup_shape(ctx: &RuntimeContextFullAccess<'_>) {
    let clock: AudioClockShim<'_> = ctx.audio_clock();
    let _sample_rate: u32 = clock.sample_rate();
    let _buffer_size: usize = clock.buffer_size();
    clock.on_tick(|tick: AudioTickContext| {
        // A real chord generator synthesizes `tick.samples_needed`
        // samples at `tick.sample_rate` and writes them downstream.
        let _ = (
            tick.timestamp_ns,
            tick.samples_needed,
            tick.sample_rate,
            tick.tick_number,
        );
    });
}

/// The restricted (`process()`) view also surfaces the clock — an audio
/// producer can read tick timing from the hot path.
#[allow(dead_code)]
fn chord_generator_process_shape(ctx: &RuntimeContextLimitedAccess<'_>) {
    let clock = ctx.audio_clock();
    let _rate = clock.sample_rate();
}

#[test]
fn media_clock_resolves_through_public_path() {
    // The ticket's second named gap: `MediaClock` must be reachable through
    // the public `sdk::media_clock` facade exactly as an external package
    // imports it — not only via the crate-internal `crate::media_clock`.
    use streamlib_plugin_sdk::sdk::media_clock::MediaClock;

    // Trivial compile-use of the type's real associated fn: `now()` returns
    // a monotonic `Duration`. Two reads are non-decreasing (monotonic), which
    // also exercises the accessor rather than merely naming the type.
    let first = MediaClock::now();
    let second = MediaClock::now();
    assert!(second >= first, "MediaClock::now() must be monotonic");
}

#[test]
fn audio_tick_context_is_engine_free_authorable() {
    // The value struct is constructible + `Copy` without any host.
    let tick = AudioTickContext {
        timestamp_ns: 1_000,
        samples_needed: 512,
        sample_rate: 48_000,
        tick_number: 3,
    };
    let copied = tick;
    assert_eq!(copied.samples_needed, 512);
    assert_eq!(tick.sample_rate, 48_000);
    assert_eq!(tick.tick_number, 3);
}

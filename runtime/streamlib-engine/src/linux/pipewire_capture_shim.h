// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1
//
// The compiled half of the PipeWire capture arm.
//
// SPA's pod builders and parsers are `static inline` C with no shared object
// behind them, so they cannot be reached by `dlopen` at all — they have to be
// compiled in. This translation unit is what compiles them, and it reaches
// libpipewire only through function pointers the Rust side filled with
// `dlsym`. It references no external symbol of its own beyond libc, which is
// what keeps `libpipewire-0.3.so.0` out of the wheel's `DT_NEEDED` set.

#ifndef STREAMLIB_PIPEWIRE_CAPTURE_SHIM_H
#define STREAMLIB_PIPEWIRE_CAPTURE_SHIM_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// The libpipewire entry points the shim calls, in the order Rust must fill
// them. One X-macro generates the typed struct, the name array Rust resolves
// against, and the count — so the three cannot drift apart, which is the whole
// reason the list is written this way rather than three times.
//
// Nothing here is newer than PipeWire 0.3.50 (`pw_stream_get_time_n` is the
// newest, and is exactly 0.3.50), so every name resolves on any host the wheel
// claims to run on.
#define STREAMLIB_PIPEWIRE_ENTRY_POINTS(ENTRY_POINT)                                               \
    ENTRY_POINT(pw_init, void, (int *argc, char ***argv))                                          \
    ENTRY_POINT(pw_get_library_version, const char *, (void))                                      \
    ENTRY_POINT(pw_thread_loop_new, struct pw_thread_loop *,                                       \
                (const char *name, const struct spa_dict *props))                                  \
    ENTRY_POINT(pw_thread_loop_destroy, void, (struct pw_thread_loop * loop))                      \
    ENTRY_POINT(pw_thread_loop_start, int, (struct pw_thread_loop * loop))                         \
    ENTRY_POINT(pw_thread_loop_stop, void, (struct pw_thread_loop * loop))                         \
    ENTRY_POINT(pw_thread_loop_lock, void, (struct pw_thread_loop * loop))                         \
    ENTRY_POINT(pw_thread_loop_unlock, void, (struct pw_thread_loop * loop))                       \
    ENTRY_POINT(pw_thread_loop_get_loop, struct pw_loop *, (struct pw_thread_loop * loop))         \
    ENTRY_POINT(pw_thread_loop_signal, void,                                                       \
                (struct pw_thread_loop * loop, bool wait_for_accept))                              \
    ENTRY_POINT(pw_thread_loop_timed_wait, int, (struct pw_thread_loop * loop, int wait_seconds))  \
    ENTRY_POINT(pw_context_new, struct pw_context *,                                               \
                (struct pw_loop * main_loop, struct pw_properties *props, size_t user_data_size))  \
    ENTRY_POINT(pw_context_destroy, void, (struct pw_context * context))                           \
    ENTRY_POINT(pw_context_connect, struct pw_core *,                                              \
                (struct pw_context * context, struct pw_properties *properties,                    \
                 size_t user_data_size))                                                           \
    ENTRY_POINT(pw_core_disconnect, int, (struct pw_core * core))                                  \
    ENTRY_POINT(pw_properties_new_dict, struct pw_properties *, (const struct spa_dict *dict))     \
    ENTRY_POINT(pw_stream_new_simple, struct pw_stream *,                                          \
                (struct pw_loop * loop, const char *name, struct pw_properties *props,             \
                 const struct pw_stream_events *events, void *data))                               \
    ENTRY_POINT(pw_stream_destroy, void, (struct pw_stream * stream))                              \
    ENTRY_POINT(pw_stream_connect, int,                                                            \
                (struct pw_stream * stream, enum pw_direction direction, uint32_t target_id,       \
                 enum pw_stream_flags flags, const struct spa_pod **params, uint32_t n_params))    \
    ENTRY_POINT(pw_stream_disconnect, int, (struct pw_stream * stream))                            \
    ENTRY_POINT(pw_stream_dequeue_buffer, struct pw_buffer *, (struct pw_stream * stream))         \
    ENTRY_POINT(pw_stream_queue_buffer, int,                                                       \
                (struct pw_stream * stream, struct pw_buffer *buffer))                             \
    ENTRY_POINT(pw_stream_get_time_n, int,                                                         \
                (struct pw_stream * stream, struct pw_time *time, size_t size))

/// How the scalars a negotiated capture stream delivers are encoded. The
/// values are the discriminants `AudioCaptureSampleFormat` is reconstructed
/// from on the Rust side.
enum StreamLibPipeWireSampleFormat {
    STREAMLIB_PIPEWIRE_SAMPLE_FORMAT_F32_LE = 0,
    STREAMLIB_PIPEWIRE_SAMPLE_FORMAT_I16_LE = 1,
};

/// How many properties [`capture_stream_properties`] declares. Shared so the
/// array and the function that fills it cannot disagree about its size.
#define STREAMLIB_PIPEWIRE_MAX_STREAM_PROPERTIES 5

/// Longest device id a caller may name. PipeWire node names are short; this is
/// generous, and a longer one is captured from as an ordinary target rather
/// than truncated into a different device's name.
#define STREAMLIB_PIPEWIRE_MAX_DEVICE_ID_BYTES 256

/// What a caller appends to a sink's name to mean "capture that sink's
/// monitor". PulseAudio's spelling, so it is the one a caller already knows.
#define STREAMLIB_PIPEWIRE_MONITOR_DEVICE_ID_SUFFIX ".monitor"

/// The sink name inside a `<sink>.monitor` device id, or 0 when the id does not
/// name a monitor.
///
/// Exposed so a test can hold the parsing without a session: getting it wrong
/// silently captures the default source, which is silence that looks like a
/// working pipeline.
size_t streamlib_pipewire_sink_name_length_of_monitor_device_id(const char *device_id);

/// How many buffers one graph cycle may hand over before the rest wait for the
/// next callback. A capture cycle normally yields exactly one; PipeWire's own
/// buffer pools are smaller than this.
#define STREAMLIB_PIPEWIRE_MAX_BUFFERS_PER_CYCLE 16

/// The byte range of a delivered chunk, clamped to what its buffer maps.
struct StreamLibPipeWireChunkExtent {
    uint32_t offset;
    uint32_t byte_count;
};

/// Clamp a daemon-supplied chunk offset and size to the mapping they index.
///
/// `spa_chunk` states the contract itself: the offset "should be taken modulo
/// the data maxsize" and the size "should be clamped to maxsize". Neither can
/// be taken on trust, because the pair becomes a Rust slice — an out-of-range
/// chunk is a read past the end of the mapping, not a bad sample value.
///
/// Exposed rather than kept private so a test can hold it without a device.
struct StreamLibPipeWireChunkExtent streamlib_pipewire_clamped_chunk_extent(uint32_t chunk_offset,
                                                                           uint32_t chunk_size,
                                                                           uint32_t data_maxsize);

/// What one capture stream settled on, fixed for the stream's lifetime.
struct StreamLibPipeWireNegotiatedCaptureFormat {
    uint32_t sample_rate;
    uint32_t channels;
    /// One of [`enum StreamLibPipeWireSampleFormat`].
    uint32_t sample_format;
};

/// What the shim calls with each block PipeWire captured.
///
/// Runs on PipeWire's thread-loop thread with that loop's lock held, so it must
/// not block and must not re-enter this shim. `interleaved_sample_bytes`
/// borrows PipeWire's mapped buffer and is invalid the moment this returns.
typedef void (*StreamLibPipeWireCapturedBlockHandOff)(void *hand_off_context,
                                                      const uint8_t *interleaved_sample_bytes,
                                                      size_t interleaved_sample_byte_count,
                                                      uint32_t sample_count,
                                                      int64_t first_sample_timestamp_ns);

/// The monotonic instant the first sample of a block arriving in one graph
/// cycle was captured.
///
/// `cycle_timestamp_ns` is `pw_time.now`, the cycle's own monotonic timestamp,
/// and `delay_in_rate_units` is `pw_time.delay` — how long a sample took to
/// travel from the capture device, expressed in `pw_time.rate` units. So
/// `now - delay` is the instant at the *end* of the quantum the cycle just
/// delivered, and the block's first sample precedes it by the block's own
/// duration. Subtracting that duration is what makes the value name the field
/// it is written into, `first_sample_timestamp_ns`.
///
/// Exposed rather than kept private so the arithmetic can be held by a test on
/// a machine with no audio server: it is the one part of this arm that is
/// wrong-by-a-quantum rather than wrong-by-a-crash.
int64_t streamlib_pipewire_first_sample_timestamp_ns(int64_t cycle_timestamp_ns,
                                                     int64_t delay_in_rate_units,
                                                     uint32_t rate_numerator,
                                                     uint32_t rate_denominator,
                                                     uint32_t sample_count, uint32_t sample_rate);

/// One opened capture stream, owned by the shim.
struct StreamLibPipeWireCaptureStream;

/// How many entry points [`streamlib_pipewire_entry_point_names`] returns.
size_t streamlib_pipewire_entry_point_count(void);

/// The entry-point names to `dlsym`, in the order the shim expects the filled
/// pointer array to carry them.
const char *const *streamlib_pipewire_entry_point_names(void);

/// Initialize libpipewire's process-global state. Call once per process,
/// before anything else here.
void streamlib_pipewire_initialize(const void *const *entry_points);

/// The version string of the libpipewire that was actually loaded, which is
/// what a probe log line should name — the vendored headers say what the API
/// looks like, not what the host shipped.
const char *streamlib_pipewire_loaded_library_version(const void *const *entry_points);

/// Whether a PipeWire daemon actually answers, by connecting a core and
/// disconnecting it.
///
/// The arm is chosen by opening rather than by loading: `libpipewire` present
/// with no daemon behind it is the common container case, and it has to demote
/// to the next arm exactly as a missing library does. Returns 0 when a daemon
/// answered, and non-zero with `failure_text` filled when it did not.
int streamlib_pipewire_daemon_answers(const void *const *entry_points, char *failure_text,
                                      size_t failure_text_capacity);

/// Open and negotiate a capture stream, blocking until PipeWire settles the
/// format or the attempt fails.
///
/// `device_id_or_null` names a PipeWire target object; NULL takes the session's
/// default source. Returns NULL with `failure_text` filled on failure. Blocks
/// arriving before `streamlib_pipewire_capture_stream_start_delivering` are
/// dequeued and recycled rather than held, so the graph's buffers keep
/// circulating while a caller is still wiring itself up.
struct StreamLibPipeWireCaptureStream *streamlib_pipewire_capture_stream_open(
    const void *const *entry_points, const char *device_id_or_null,
    struct StreamLibPipeWireNegotiatedCaptureFormat *negotiated_format_out, char *failure_text,
    size_t failure_text_capacity);

/// Begin handing captured blocks to `hand_off`, replacing any earlier one.
void streamlib_pipewire_capture_stream_start_delivering(
    struct StreamLibPipeWireCaptureStream *capture_stream,
    StreamLibPipeWireCapturedBlockHandOff hand_off, void *hand_off_context);

/// Stop handing off. The hand-off is not called again once this returns.
void streamlib_pipewire_capture_stream_stop_delivering(
    struct StreamLibPipeWireCaptureStream *capture_stream);

/// Disconnect, tear down and free the stream. Safe on NULL.
void streamlib_pipewire_capture_stream_close(
    struct StreamLibPipeWireCaptureStream *capture_stream);

#ifdef __cplusplus
}
#endif

#endif /* STREAMLIB_PIPEWIRE_CAPTURE_SHIM_H */

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#include "pipewire_capture_shim.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <pipewire/keys.h>
#include <pipewire/properties.h>
#include <pipewire/stream.h>
#include <pipewire/thread-loop.h>
#include <spa/param/audio/format-utils.h>
#include <spa/pod/builder.h>
#include <spa/utils/dict.h>

// Nothing in this file may call a `pw_*` or `spa_*` symbol directly. Everything
// libpipewire exports is reached through `entry_points`; everything SPA offers
// is `static inline` and compiles in. `nm -u` on the object file naming any
// symbol beyond libc is the failure this arm exists to prevent, and
// `test_wheel_portability.py` is what catches it.
//
// The `pw_log_*` macros are the easy way to break that, since they expand to a
// call to the exported `pw_log_logt`. Diagnostics here go into the caller's
// `failure_text` buffer, and the Rust side emits them through `tracing`.

/// The typed view of the pointer array Rust fills.
struct StreamLibPipeWireEntryPoints {
#define STREAMLIB_PIPEWIRE_DECLARE_ENTRY_POINT(name, return_type, parameters)                      \
    return_type (*name) parameters;
    STREAMLIB_PIPEWIRE_ENTRY_POINTS(STREAMLIB_PIPEWIRE_DECLARE_ENTRY_POINT)
#undef STREAMLIB_PIPEWIRE_DECLARE_ENTRY_POINT
};

static const char *const kEntryPointNames[] = {
#define STREAMLIB_PIPEWIRE_NAME_ENTRY_POINT(name, return_type, parameters) #name,
    STREAMLIB_PIPEWIRE_ENTRY_POINTS(STREAMLIB_PIPEWIRE_NAME_ENTRY_POINT)
#undef STREAMLIB_PIPEWIRE_NAME_ENTRY_POINT
};

// Rust writes the table as a flat array of `dlsym` results and never names a
// field, so "one pointer per name, in order" is the whole contract between the
// two halves — and `copy_entry_points` memcpy's `sizeof(struct)` bytes out of
// that array. This is what pins the two to the same length: padding, or a
// compiler that sized a member differently, would make that read run past the
// end of what Rust allocated.
_Static_assert(sizeof(struct StreamLibPipeWireEntryPoints) ==
                   sizeof(kEntryPointNames) / sizeof(kEntryPointNames[0]) * sizeof(void (*)(void)),
               "the entry-point struct must be exactly one function pointer per resolved name");

/// The device's own sample rate and channel count are what this arm publishes:
/// the port window contract and its resampler are a later rung, so asking
/// PipeWire to convert either one would be resampling nothing declared. Only
/// the scalar encoding is pinned, and it is pinned little-endian rather than
/// host-endian because `AudioBlock.samples` is little-endian by wire contract.
#define STREAMLIB_PIPEWIRE_REQUESTED_SAMPLE_FORMAT SPA_AUDIO_FORMAT_F32_LE

/// How long `open` waits for PipeWire to settle a format before giving up. A
/// session that has not answered in this long is not going to, and the caller
/// has another arm to demote to.
#define STREAMLIB_PIPEWIRE_NEGOTIATION_TIMEOUT_SECONDS 5

#define STREAMLIB_PIPEWIRE_POD_BUILDER_CAPACITY 1024

struct StreamLibPipeWireCaptureStream {
    struct StreamLibPipeWireEntryPoints entry_points;
    struct pw_thread_loop *thread_loop;
    struct pw_stream *stream;

    /// Filled by the format callback on the loop thread, read by `open` once
    /// negotiation has been signalled.
    struct StreamLibPipeWireNegotiatedCaptureFormat negotiated_format;
    bool format_was_negotiated;
    /// Set when the stream reaches `PW_STREAM_STATE_ERROR`, so a caller
    /// waiting for a format stops waiting rather than sitting out the timeout.
    bool stream_failed;
    char stream_failure_text[256];

    /// NULL until a caller starts delivering.
    ///
    /// Read and written only with the thread loop's lock held. That holds for
    /// the reader because this stream deliberately does not set
    /// `PW_STREAM_FLAG_RT_PROCESS` — see `connect_capture_stream`.
    StreamLibPipeWireCapturedBlockHandOff hand_off;
    void *hand_off_context;
};

static void copy_failure_text(char *failure_text, size_t failure_text_capacity, const char *text)
{
    if (failure_text == NULL || failure_text_capacity == 0)
        return;
    snprintf(failure_text, failure_text_capacity, "%s", text);
}

static void copy_entry_points(struct StreamLibPipeWireEntryPoints *into,
                              const void *const *entry_points)
{
    memcpy(into, entry_points, sizeof(*into));
}

/// Bytes one scalar of a wire dtype occupies.
///
/// Refuses anything that is not one of the two declared encodings rather than
/// defaulting to a width, because this multiplies into the frame stride that
/// slices the delivered buffer: a guessed width reads the wrong bytes instead
/// of failing.
static bool bytes_per_scalar_of(uint32_t sample_format, uint32_t *bytes_per_scalar_out)
{
    switch (sample_format) {
    case STREAMLIB_PIPEWIRE_SAMPLE_FORMAT_F32_LE:
        *bytes_per_scalar_out = 4;
        return true;
    case STREAMLIB_PIPEWIRE_SAMPLE_FORMAT_I16_LE:
        *bytes_per_scalar_out = 2;
        return true;
    default:
        return false;
    }
}

/// SPA's encoding for the scalars, mapped onto the wire's two legal dtypes.
/// Only the little-endian spellings are accepted: `AudioBlock.samples` is
/// little-endian by contract, so a big-endian negotiation is a refusal rather
/// than something to byte-swap silently.
static bool wire_sample_format_of(uint32_t spa_audio_format, uint32_t *sample_format_out)
{
    switch (spa_audio_format) {
    case SPA_AUDIO_FORMAT_F32_LE:
        *sample_format_out = STREAMLIB_PIPEWIRE_SAMPLE_FORMAT_F32_LE;
        return true;
    case SPA_AUDIO_FORMAT_S16_LE:
        *sample_format_out = STREAMLIB_PIPEWIRE_SAMPLE_FORMAT_I16_LE;
        return true;
    default:
        return false;
    }
}

/// Nanoseconds `sample_count` per-channel samples occupy at `sample_rate`,
/// computed wide so a long block cannot overflow the intermediate.
static int64_t nanoseconds_occupied_by(uint64_t sample_count, uint32_t sample_rate)
{
    if (sample_rate == 0)
        return 0;
    return (int64_t)((sample_count * UINT64_C(1000000000)) / (uint64_t)sample_rate);
}

struct StreamLibPipeWireChunkExtent streamlib_pipewire_clamped_chunk_extent(uint32_t chunk_offset,
                                                                           uint32_t chunk_size,
                                                                           uint32_t data_maxsize)
{
    struct StreamLibPipeWireChunkExtent extent = {0, 0};
    if (data_maxsize == 0)
        return extent;
    extent.offset = chunk_offset % data_maxsize;
    uint32_t bytes_after_the_offset = data_maxsize - extent.offset;
    extent.byte_count = chunk_size < bytes_after_the_offset ? chunk_size : bytes_after_the_offset;
    return extent;
}

int64_t streamlib_pipewire_first_sample_timestamp_ns(int64_t cycle_timestamp_ns,
                                                     int64_t delay_in_rate_units,
                                                     uint32_t rate_numerator,
                                                     uint32_t rate_denominator,
                                                     uint32_t sample_count, uint32_t sample_rate)
{
    int64_t delay_ns = 0;
    if (rate_denominator != 0) {
        delay_ns = delay_in_rate_units * (int64_t)rate_numerator * INT64_C(1000000000) /
                   (int64_t)rate_denominator;
    }
    return cycle_timestamp_ns - delay_ns - nanoseconds_occupied_by(sample_count, sample_rate);
}

static int64_t first_sample_timestamp_of(const struct pw_time *time, uint32_t sample_count,
                                         uint32_t sample_rate)
{
    return streamlib_pipewire_first_sample_timestamp_ns(time->now, time->delay, time->rate.num,
                                                        time->rate.denom, sample_count,
                                                        sample_rate);
}

/// End the stream with a reason, and wake whoever is waiting on negotiation.
static void fail_the_stream(struct StreamLibPipeWireCaptureStream *capture_stream,
                            const char *reason)
{
    capture_stream->stream_failed = true;
    snprintf(capture_stream->stream_failure_text, sizeof(capture_stream->stream_failure_text), "%s",
             reason);
    capture_stream->entry_points.pw_thread_loop_signal(capture_stream->thread_loop, false);
}

static void on_stream_state_changed(void *data, enum pw_stream_state old_state,
                                    enum pw_stream_state state, const char *error)
{
    struct StreamLibPipeWireCaptureStream *capture_stream = data;
    (void)old_state;
    if (state != PW_STREAM_STATE_ERROR)
        return;
    fail_the_stream(capture_stream, error != NULL ? error : "the stream entered its error state");
}

static void on_stream_param_changed(void *data, uint32_t id, const struct spa_pod *param)
{
    struct StreamLibPipeWireCaptureStream *capture_stream = data;
    struct spa_audio_info audio_info;
    char reason[sizeof(capture_stream->stream_failure_text)];
    uint32_t sample_format = 0;

    if (param == NULL || id != SPA_PARAM_Format)
        return;

    memset(&audio_info, 0, sizeof(audio_info));
    if (spa_format_parse(param, &audio_info.media_type, &audio_info.media_subtype) < 0)
        return;
    if (audio_info.media_type != SPA_MEDIA_TYPE_audio ||
        audio_info.media_subtype != SPA_MEDIA_SUBTYPE_raw)
        return;
    if (spa_format_audio_raw_parse(param, &audio_info.info.raw) < 0)
        return;

    if (!wire_sample_format_of(audio_info.info.raw.format, &sample_format)) {
        snprintf(reason, sizeof(reason),
                 "PipeWire negotiated SPA audio format %u, which is not one of the two "
                 "little-endian encodings an AudioBlock can carry",
                 audio_info.info.raw.format);
        fail_the_stream(capture_stream, reason);
        return;
    }
    if (audio_info.info.raw.rate == 0 || audio_info.info.raw.channels == 0) {
        snprintf(reason, sizeof(reason),
                 "PipeWire negotiated %u Hz and %u channels, and no block duration derives "
                 "from either being zero",
                 audio_info.info.raw.rate, audio_info.info.raw.channels);
        fail_the_stream(capture_stream, reason);
        return;
    }

    // A renegotiation after `open` returned would leave the caller framing
    // blocks by a rate and channel count nothing told it about — mis-sized
    // samples and mis-timed stamps rather than a failure. The seam states the
    // format is fixed for the stream's lifetime, so a change ends the stream
    // instead of quietly moving underneath it.
    if (capture_stream->format_was_negotiated) {
        if (capture_stream->negotiated_format.sample_rate != audio_info.info.raw.rate ||
            capture_stream->negotiated_format.channels != audio_info.info.raw.channels ||
            capture_stream->negotiated_format.sample_format != sample_format) {
            snprintf(reason, sizeof(reason),
                     "PipeWire renegotiated this capture stream from %u Hz / %u channels / "
                     "dtype %u to %u Hz / %u channels / dtype %u, and a stream's format is "
                     "fixed for its lifetime",
                     capture_stream->negotiated_format.sample_rate,
                     capture_stream->negotiated_format.channels,
                     capture_stream->negotiated_format.sample_format, audio_info.info.raw.rate,
                     audio_info.info.raw.channels, sample_format);
            fail_the_stream(capture_stream, reason);
        }
        return;
    }

    capture_stream->negotiated_format.sample_rate = audio_info.info.raw.rate;
    capture_stream->negotiated_format.channels = audio_info.info.raw.channels;
    capture_stream->negotiated_format.sample_format = sample_format;
    capture_stream->format_was_negotiated = true;
    capture_stream->entry_points.pw_thread_loop_signal(capture_stream->thread_loop, false);
}

/// One dequeued buffer's payload, resolved against its own mapping.
struct CapturedBufferPayload {
    struct pw_buffer *buffer;
    const uint8_t *samples;
    size_t sample_byte_count;
    uint32_t sample_count;
};

/// Whatever `datas[0]` of a dequeued buffer actually maps, with the daemon's
/// chunk geometry clamped to it and any partial trailing frame dropped.
static struct CapturedBufferPayload payload_of(struct pw_buffer *buffer, uint32_t bytes_per_frame)
{
    struct CapturedBufferPayload payload = {buffer, NULL, 0, 0};
    struct spa_data *data_plane = &buffer->buffer->datas[0];

    if (data_plane->data == NULL || data_plane->chunk == NULL ||
        SPA_FLAG_IS_SET(data_plane->chunk->flags, SPA_CHUNK_FLAG_CORRUPTED))
        return payload;

    // The daemon supplies the offset and the size, and `spa_chunk` states that
    // the first is taken modulo `maxsize` and the second clamped to it. Neither
    // is taken at face value here: this pair becomes a Rust slice, so an
    // out-of-range chunk would be a read past the end of the mapping rather
    // than a bad sample value.
    struct StreamLibPipeWireChunkExtent extent = streamlib_pipewire_clamped_chunk_extent(
        data_plane->chunk->offset, data_plane->chunk->size, data_plane->maxsize);
    payload.samples = (const uint8_t *)data_plane->data + extent.offset;
    // A partial trailing frame cannot be described by a per-channel sample
    // count, so what is handed off is the whole frames only.
    payload.sample_count = extent.byte_count / bytes_per_frame;
    payload.sample_byte_count = (size_t)payload.sample_count * bytes_per_frame;
    return payload;
}

static void on_stream_process(void *data)
{
    struct StreamLibPipeWireCaptureStream *capture_stream = data;
    const struct StreamLibPipeWireEntryPoints *entry_points = &capture_stream->entry_points;
    const struct StreamLibPipeWireNegotiatedCaptureFormat *format =
        &capture_stream->negotiated_format;
    // A capture cycle normally yields exactly one buffer, and this array is
    // sized well past that so the ordinary case never takes a second pass.
    struct CapturedBufferPayload payloads[STREAMLIB_PIPEWIRE_MAX_BUFFERS_PER_CYCLE];
    uint32_t payload_count = 0;
    uint64_t samples_in_this_cycle = 0;
    uint32_t bytes_per_scalar = 0;
    struct pw_buffer *buffer;

    // Zero until the format settles, and a stride of zero divides nothing.
    if (!bytes_per_scalar_of(format->sample_format, &bytes_per_scalar))
        return;
    uint32_t bytes_per_frame = bytes_per_scalar * format->channels;
    if (bytes_per_frame == 0)
        return;

    // Dequeued before any is handed off, because every buffer a cycle delivers
    // ends at the same instant — `now - delay` — so the first one's timestamp
    // is only knowable once the whole cycle's sample count is. Stamping
    // forwards from the first block instead would put every later block in the
    // future relative to the moment its samples were actually captured.
    while (payload_count < STREAMLIB_PIPEWIRE_MAX_BUFFERS_PER_CYCLE &&
           (buffer = entry_points->pw_stream_dequeue_buffer(capture_stream->stream)) != NULL) {
        payloads[payload_count] = payload_of(buffer, bytes_per_frame);
        samples_in_this_cycle += payloads[payload_count].sample_count;
        payload_count++;
    }

    if (payload_count > 0 && capture_stream->hand_off != NULL && samples_in_this_cycle > 0) {
        struct pw_time time;
        memset(&time, 0, sizeof(time));
        entry_points->pw_stream_get_time_n(capture_stream->stream, &time, sizeof(time));

        int64_t next_timestamp_ns = first_sample_timestamp_of(
            &time, (uint32_t)samples_in_this_cycle, format->sample_rate);
        for (uint32_t index = 0; index < payload_count; index++) {
            if (payloads[index].sample_count == 0)
                continue;
            capture_stream->hand_off(capture_stream->hand_off_context, payloads[index].samples,
                                     payloads[index].sample_byte_count,
                                     payloads[index].sample_count, next_timestamp_ns);
            next_timestamp_ns +=
                nanoseconds_occupied_by(payloads[index].sample_count, format->sample_rate);
        }
    }

    for (uint32_t index = 0; index < payload_count; index++)
        entry_points->pw_stream_queue_buffer(capture_stream->stream, payloads[index].buffer);
}

// Version 0 rather than `PW_VERSION_STREAM_EVENTS`: it covers every callback
// this arm installs, and declaring only what is implemented is what lets a
// libpipewire older than the vendored headers dispatch against this struct
// safely.
static const struct pw_stream_events kCaptureStreamEvents = {
    .version = 0,
    .state_changed = on_stream_state_changed,
    .param_changed = on_stream_param_changed,
    .process = on_stream_process,
};

size_t streamlib_pipewire_entry_point_count(void)
{
    return sizeof(kEntryPointNames) / sizeof(kEntryPointNames[0]);
}

const char *const *streamlib_pipewire_entry_point_names(void)
{
    return kEntryPointNames;
}

void streamlib_pipewire_initialize(const void *const *entry_points)
{
    struct StreamLibPipeWireEntryPoints resolved;
    copy_entry_points(&resolved, entry_points);
    resolved.pw_init(NULL, NULL);
}

const char *streamlib_pipewire_loaded_library_version(const void *const *entry_points)
{
    struct StreamLibPipeWireEntryPoints resolved;
    copy_entry_points(&resolved, entry_points);
    return resolved.pw_get_library_version();
}

int streamlib_pipewire_daemon_answers(const void *const *entry_points, char *failure_text,
                                      size_t failure_text_capacity)
{
    struct StreamLibPipeWireEntryPoints resolved;
    copy_entry_points(&resolved, entry_points);

    struct pw_thread_loop *thread_loop = resolved.pw_thread_loop_new("streamlib-audio-probe", NULL);
    if (thread_loop == NULL) {
        copy_failure_text(failure_text, failure_text_capacity,
                          "libpipewire loaded but would not create a thread loop");
        return 1;
    }

    int verdict = 0;
    resolved.pw_thread_loop_lock(thread_loop);
    if (resolved.pw_thread_loop_start(thread_loop) < 0) {
        copy_failure_text(failure_text, failure_text_capacity,
                          "libpipewire loaded but its thread loop would not start");
        verdict = 1;
    } else {
        struct pw_context *context =
            resolved.pw_context_new(resolved.pw_thread_loop_get_loop(thread_loop), NULL, 0);
        if (context == NULL) {
            copy_failure_text(failure_text, failure_text_capacity,
                              "libpipewire loaded but would not create a context");
            verdict = 1;
        } else {
            struct pw_core *core = resolved.pw_context_connect(context, NULL, 0);
            if (core == NULL) {
                copy_failure_text(failure_text, failure_text_capacity,
                                  "libpipewire loaded but no PipeWire daemon answered");
                verdict = 1;
            } else {
                resolved.pw_core_disconnect(core);
            }
            resolved.pw_context_destroy(context);
        }
    }
    resolved.pw_thread_loop_unlock(thread_loop);
    resolved.pw_thread_loop_stop(thread_loop);
    resolved.pw_thread_loop_destroy(thread_loop);
    return verdict;
}

/// Free everything the stream holds. The caller must not hold the thread loop's
/// lock — this takes it, and stopping the loop joins its thread.
///
/// The one teardown, reached by every failure path in
/// `streamlib_pipewire_capture_stream_open` as well as by close: a hand-rolled
/// subset at one exit is how that exit ends up skipping a step the others take.
/// Tolerates a NULL stream and a loop that was never started.
static void destroy_capture_stream(struct StreamLibPipeWireCaptureStream *capture_stream)
{
    if (capture_stream->stream != NULL) {
        capture_stream->entry_points.pw_thread_loop_lock(capture_stream->thread_loop);
        capture_stream->entry_points.pw_stream_disconnect(capture_stream->stream);
        capture_stream->entry_points.pw_stream_destroy(capture_stream->stream);
        capture_stream->stream = NULL;
        capture_stream->entry_points.pw_thread_loop_unlock(capture_stream->thread_loop);
    }
    if (capture_stream->thread_loop != NULL) {
        capture_stream->entry_points.pw_thread_loop_stop(capture_stream->thread_loop);
        capture_stream->entry_points.pw_thread_loop_destroy(capture_stream->thread_loop);
        capture_stream->thread_loop = NULL;
    }
    free(capture_stream);
}

size_t streamlib_pipewire_sink_name_length_of_monitor_device_id(const char *device_id)
{
    if (device_id == NULL)
        return 0;
    size_t length = strlen(device_id);
    size_t suffix_length = strlen(STREAMLIB_PIPEWIRE_MONITOR_DEVICE_ID_SUFFIX);
    // A device id that is *only* the suffix names no sink, so it is left alone
    // to fail as the ordinary missing target it is.
    if (length <= suffix_length)
        return 0;
    if (strcmp(device_id + length - suffix_length,
               STREAMLIB_PIPEWIRE_MONITOR_DEVICE_ID_SUFFIX) != 0)
        return 0;
    return length - suffix_length;
}

/// The properties a capture stream announces itself with. `PW_KEY_TARGET_OBJECT`
/// is only set when a caller named a device: absent, the session routes the
/// stream to its own default source.
///
/// Takes the array's capacity rather than trusting the caller to have sized it,
/// because a property added here would otherwise overwrite the caller's stack
/// silently.
static uint32_t capture_stream_properties(struct spa_dict_item *items, uint32_t item_capacity,
                                          const char *device_id_or_null,
                                          char *sink_name, size_t sink_name_capacity)
{
    uint32_t count = 0;
    if (item_capacity < STREAMLIB_PIPEWIRE_MAX_STREAM_PROPERTIES)
        return 0;
    items[count++] = SPA_DICT_ITEM_INIT(PW_KEY_MEDIA_TYPE, "Audio");
    items[count++] = SPA_DICT_ITEM_INIT(PW_KEY_MEDIA_CATEGORY, "Capture");
    items[count++] = SPA_DICT_ITEM_INIT(PW_KEY_MEDIA_ROLE, "Production");
    if (device_id_or_null == NULL)
        return count;

    // A sink's monitor is a capture endpoint the session already routes, and
    // `stream.capture.sink` is the only way to reach one — targeting a sink
    // without it attaches to the default source instead, which is silence that
    // looks like success. The `.monitor` spelling is PulseAudio's, so it is the
    // one a caller already knows, and it needs no configuration dial of its own.
    size_t sink_name_length = streamlib_pipewire_sink_name_length_of_monitor_device_id(
        device_id_or_null);
    if (sink_name_length > 0 && sink_name_length < sink_name_capacity) {
        memcpy(sink_name, device_id_or_null, sink_name_length);
        sink_name[sink_name_length] = '\0';
        items[count++] = SPA_DICT_ITEM_INIT(PW_KEY_TARGET_OBJECT, sink_name);
        items[count++] = SPA_DICT_ITEM_INIT(PW_KEY_STREAM_CAPTURE_SINK, "true");
        return count;
    }

    items[count++] = SPA_DICT_ITEM_INIT(PW_KEY_TARGET_OBJECT, device_id_or_null);
    return count;
}

/// Connect the stream for capture.
///
/// `PW_STREAM_FLAG_RT_PROCESS` must stay unset. With it, `process` runs on
/// PipeWire's realtime data thread, which does not hold the thread loop's lock
/// — and that lock is what makes installing and retiring a hand-off safe
/// against a callback that is already running.
///
/// `PW_STREAM_FLAG_DONT_RECONNECT` is what makes a named device authoritative:
/// `PW_STREAM_FLAG_AUTOCONNECT` alone treats a target it cannot resolve as
/// licence to link the session default instead, and capturing a device other
/// than the one the caller named is worse than failing. Nothing is set when no
/// device was named — there the session default is what was asked for.
static int connect_capture_stream(struct StreamLibPipeWireCaptureStream *capture_stream,
                                  const char *device_id_or_null, const struct spa_pod **params,
                                  uint32_t param_count)
{
    enum pw_stream_flags flags = PW_STREAM_FLAG_AUTOCONNECT | PW_STREAM_FLAG_MAP_BUFFERS;
    if (device_id_or_null != NULL)
        flags |= PW_STREAM_FLAG_DONT_RECONNECT;
    return capture_stream->entry_points.pw_stream_connect(
        capture_stream->stream, PW_DIRECTION_INPUT, PW_ID_ANY, flags, params, param_count);
}

struct StreamLibPipeWireCaptureStream *streamlib_pipewire_capture_stream_open(
    const void *const *entry_points, const char *device_id_or_null,
    struct StreamLibPipeWireNegotiatedCaptureFormat *negotiated_format_out, char *failure_text,
    size_t failure_text_capacity)
{
    uint8_t pod_storage[STREAMLIB_PIPEWIRE_POD_BUILDER_CAPACITY];
    struct spa_pod_builder pod_builder = SPA_POD_BUILDER_INIT(pod_storage, sizeof(pod_storage));
    struct spa_audio_info_raw requested_format = {
        .format = STREAMLIB_PIPEWIRE_REQUESTED_SAMPLE_FORMAT,
    };
    struct spa_dict_item property_items[STREAMLIB_PIPEWIRE_MAX_STREAM_PROPERTIES];
    // Outlives the dict, which borrows it rather than copying.
    char monitored_sink_name[STREAMLIB_PIPEWIRE_MAX_DEVICE_ID_BYTES];
    const struct spa_pod *params[1];
    const struct StreamLibPipeWireEntryPoints *resolved;
    struct pw_properties *stream_properties;
    struct spa_dict properties;
    bool thread_loop_is_locked = false;
    const char *failure_reason;
    char connect_failure_reason[128];
    int connect_result;

    struct StreamLibPipeWireCaptureStream *capture_stream = calloc(1, sizeof(*capture_stream));
    if (capture_stream == NULL) {
        copy_failure_text(failure_text, failure_text_capacity,
                          "out of memory opening a PipeWire capture stream");
        return NULL;
    }
    copy_entry_points(&capture_stream->entry_points, entry_points);
    resolved = &capture_stream->entry_points;

    capture_stream->thread_loop = resolved->pw_thread_loop_new("streamlib-audio-capture", NULL);
    if (capture_stream->thread_loop == NULL) {
        failure_reason = "PipeWire would not create a capture thread loop";
        goto fail;
    }

    params[0] = spa_format_audio_raw_build(&pod_builder, SPA_PARAM_EnumFormat, &requested_format);
    properties = (struct spa_dict)SPA_DICT_INIT(
        property_items,
        capture_stream_properties(property_items, STREAMLIB_PIPEWIRE_MAX_STREAM_PROPERTIES,
                                  device_id_or_null, monitored_sink_name,
                                  sizeof(monitored_sink_name)));

    resolved->pw_thread_loop_lock(capture_stream->thread_loop);
    thread_loop_is_locked = true;

    if (resolved->pw_thread_loop_start(capture_stream->thread_loop) < 0) {
        failure_reason = "PipeWire's capture thread loop would not start";
        goto fail;
    }

    // `pw_stream_new_simple` takes ownership of the properties, including when
    // it fails, so there is no path here that has to free them.
    stream_properties = resolved->pw_properties_new_dict(&properties);
    capture_stream->stream = resolved->pw_stream_new_simple(
        resolved->pw_thread_loop_get_loop(capture_stream->thread_loop), "streamlib-capture",
        stream_properties, &kCaptureStreamEvents, capture_stream);
    if (capture_stream->stream == NULL) {
        failure_reason = "PipeWire would not create a capture stream";
        goto fail;
    }

    connect_result = connect_capture_stream(capture_stream, device_id_or_null, params, 1);
    if (connect_result < 0) {
        snprintf(connect_failure_reason, sizeof(connect_failure_reason),
                 "PipeWire refused the capture connection (%d)", connect_result);
        failure_reason = connect_failure_reason;
        goto fail;
    }

    // Negotiation is what makes the stream's format knowable, and a caller
    // cannot size a block without it — so `open` is where the wait belongs
    // rather than the first callback.
    while (!capture_stream->format_was_negotiated && !capture_stream->stream_failed) {
        if (resolved->pw_thread_loop_timed_wait(capture_stream->thread_loop,
                                                STREAMLIB_PIPEWIRE_NEGOTIATION_TIMEOUT_SECONDS) !=
            0) {
            snprintf(capture_stream->stream_failure_text,
                     sizeof(capture_stream->stream_failure_text),
                     "PipeWire settled no capture format within %d seconds%s",
                     STREAMLIB_PIPEWIRE_NEGOTIATION_TIMEOUT_SECONDS,
                     device_id_or_null != NULL
                         ? ", which is what a device id naming no node in the session graph "
                           "looks like"
                         : "");
            capture_stream->stream_failed = true;
        }
    }
    if (capture_stream->stream_failed) {
        failure_reason = capture_stream->stream_failure_text;
        goto fail;
    }

    resolved->pw_thread_loop_unlock(capture_stream->thread_loop);

    if (negotiated_format_out != NULL)
        *negotiated_format_out = capture_stream->negotiated_format;
    return capture_stream;

fail:
    // Copied before the teardown, because the reason may live in the struct the
    // teardown is about to free.
    copy_failure_text(failure_text, failure_text_capacity, failure_reason);
    if (thread_loop_is_locked)
        resolved->pw_thread_loop_unlock(capture_stream->thread_loop);
    destroy_capture_stream(capture_stream);
    return NULL;
}

void streamlib_pipewire_capture_stream_start_delivering(
    struct StreamLibPipeWireCaptureStream *capture_stream,
    StreamLibPipeWireCapturedBlockHandOff hand_off, void *hand_off_context)
{
    // Under the loop lock because `process` reads both fields holding that same
    // lock, so the pair is swapped atomically with respect to it — a hand-off
    // paired with the previous caller's context is a use-after-free rather than
    // a lost block.
    capture_stream->entry_points.pw_thread_loop_lock(capture_stream->thread_loop);
    capture_stream->hand_off_context = hand_off_context;
    capture_stream->hand_off = hand_off;
    capture_stream->entry_points.pw_thread_loop_unlock(capture_stream->thread_loop);
}

void streamlib_pipewire_capture_stream_stop_delivering(
    struct StreamLibPipeWireCaptureStream *capture_stream)
{
    // Taking the lock is what makes "the hand-off is not called again once this
    // returns" true: `process` runs on the loop thread holding this same lock,
    // so an in-flight callback owns it and this blocks until that callback has
    // finished. It is why `PW_STREAM_FLAG_RT_PROCESS` is not set — see
    // `connect_capture_stream`.
    capture_stream->entry_points.pw_thread_loop_lock(capture_stream->thread_loop);
    capture_stream->hand_off = NULL;
    capture_stream->hand_off_context = NULL;
    capture_stream->entry_points.pw_thread_loop_unlock(capture_stream->thread_loop);
}

void streamlib_pipewire_capture_stream_close(struct StreamLibPipeWireCaptureStream *capture_stream)
{
    if (capture_stream == NULL)
        return;
    streamlib_pipewire_capture_stream_stop_delivering(capture_stream);
    destroy_capture_stream(capture_stream);
}

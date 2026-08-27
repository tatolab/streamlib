// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#include "pipewire_capture_shim.h"

#include <stdlib.h>
#include <stdio.h>
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
// two halves. If a future entry point were ever not a plain function pointer
// this would stop compiling, which is the point.
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

    /// NULL until a caller starts delivering. Read and written only under the
    /// thread loop's lock, which the process callback already holds.
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

static uint32_t bytes_per_scalar_of(uint32_t sample_format)
{
    return sample_format == STREAMLIB_PIPEWIRE_SAMPLE_FORMAT_I16_LE ? 2 : 4;
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

int64_t streamlib_pipewire_first_sample_timestamp_ns(int64_t cycle_timestamp_ns,
                                                     int64_t delay_in_rate_units,
                                                     uint32_t rate_numerator,
                                                     uint32_t rate_denominator,
                                                     uint32_t sample_count, uint32_t sample_rate)
{
    int64_t delay_ns = 0;
    if (rate_denominator != 0) {
        delay_ns = (int64_t)((double)delay_in_rate_units * 1e9 * (double)rate_numerator /
                             (double)rate_denominator);
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

static void on_stream_state_changed(void *data, enum pw_stream_state old_state,
                                    enum pw_stream_state state, const char *error)
{
    struct StreamLibPipeWireCaptureStream *capture_stream = data;
    (void)old_state;
    if (state != PW_STREAM_STATE_ERROR)
        return;
    capture_stream->stream_failed = true;
    snprintf(capture_stream->stream_failure_text, sizeof(capture_stream->stream_failure_text),
             "%s", error != NULL ? error : "the stream entered its error state");
    capture_stream->entry_points.pw_thread_loop_signal(capture_stream->thread_loop, false);
}

static void on_stream_param_changed(void *data, uint32_t id, const struct spa_pod *param)
{
    struct StreamLibPipeWireCaptureStream *capture_stream = data;
    struct spa_audio_info audio_info;

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

    uint32_t sample_format = 0;
    if (!wire_sample_format_of(audio_info.info.raw.format, &sample_format)) {
        capture_stream->stream_failed = true;
        snprintf(capture_stream->stream_failure_text,
                 sizeof(capture_stream->stream_failure_text),
                 "PipeWire negotiated SPA audio format %u, which is not one of the two "
                 "little-endian encodings an AudioBlock can carry",
                 audio_info.info.raw.format);
    } else if (audio_info.info.raw.rate == 0 || audio_info.info.raw.channels == 0) {
        capture_stream->stream_failed = true;
        snprintf(capture_stream->stream_failure_text,
                 sizeof(capture_stream->stream_failure_text),
                 "PipeWire negotiated %u Hz and %u channels, and no block duration derives "
                 "from either being zero",
                 audio_info.info.raw.rate, audio_info.info.raw.channels);
    } else {
        capture_stream->negotiated_format.sample_rate = audio_info.info.raw.rate;
        capture_stream->negotiated_format.channels = audio_info.info.raw.channels;
        capture_stream->negotiated_format.sample_format = sample_format;
        capture_stream->format_was_negotiated = true;
    }
    capture_stream->entry_points.pw_thread_loop_signal(capture_stream->thread_loop, false);
}

static void on_stream_process(void *data)
{
    struct StreamLibPipeWireCaptureStream *capture_stream = data;
    const struct StreamLibPipeWireEntryPoints *entry_points = &capture_stream->entry_points;
    const struct StreamLibPipeWireNegotiatedCaptureFormat *format =
        &capture_stream->negotiated_format;
    struct pw_time time;
    bool time_was_read = false;
    // A capture cycle normally yields exactly one buffer. When it yields more,
    // each one after the first continues from where the previous ended rather
    // than re-reading the cycle's single `now` — otherwise N blocks would claim
    // one instant, which is the same mistake a timerfd catch-up burst invites.
    int64_t next_timestamp_ns = 0;
    bool next_timestamp_is_known = false;
    uint32_t bytes_per_frame = bytes_per_scalar_of(format->sample_format) * format->channels;

    struct pw_buffer *buffer;
    while ((buffer = entry_points->pw_stream_dequeue_buffer(capture_stream->stream)) != NULL) {
        struct spa_data *data_plane = &buffer->buffer->datas[0];
        uint32_t sample_count = 0;
        const uint8_t *samples = NULL;
        size_t sample_byte_count = 0;

        if (data_plane->data != NULL && data_plane->chunk != NULL && bytes_per_frame != 0 &&
            !SPA_FLAG_IS_SET(data_plane->chunk->flags, SPA_CHUNK_FLAG_CORRUPTED)) {
            sample_byte_count = data_plane->chunk->size;
            samples = (const uint8_t *)data_plane->data + data_plane->chunk->offset;
            sample_count = (uint32_t)(sample_byte_count / bytes_per_frame);
            // A partial trailing frame cannot be described by a per-channel
            // sample count, so what is handed off is the whole frames only.
            sample_byte_count = (size_t)sample_count * bytes_per_frame;
        }

        if (sample_count > 0 && capture_stream->hand_off != NULL) {
            if (!time_was_read) {
                memset(&time, 0, sizeof(time));
                entry_points->pw_stream_get_time_n(capture_stream->stream, &time, sizeof(time));
                time_was_read = true;
            }
            if (!next_timestamp_is_known) {
                next_timestamp_ns =
                    first_sample_timestamp_of(&time, sample_count, format->sample_rate);
                next_timestamp_is_known = true;
            }
            capture_stream->hand_off(capture_stream->hand_off_context, samples, sample_byte_count,
                                     sample_count, next_timestamp_ns);
            next_timestamp_ns += nanoseconds_occupied_by(sample_count, format->sample_rate);
        }

        entry_points->pw_stream_queue_buffer(capture_stream->stream, buffer);
    }
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

/// Free everything the stream holds. The thread loop must already be stopped
/// or unlocked by the caller — every path here is off the loop thread.
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

/// The properties a capture stream announces itself with. `PW_KEY_TARGET_OBJECT`
/// is only set when a caller named a device: absent, the session routes the
/// stream to its own default source.
static uint32_t capture_stream_properties(struct spa_dict_item *items, const char *device_id_or_null)
{
    uint32_t count = 0;
    items[count++] = SPA_DICT_ITEM_INIT(PW_KEY_MEDIA_TYPE, "Audio");
    items[count++] = SPA_DICT_ITEM_INIT(PW_KEY_MEDIA_CATEGORY, "Capture");
    items[count++] = SPA_DICT_ITEM_INIT(PW_KEY_MEDIA_ROLE, "Production");
    if (device_id_or_null != NULL)
        items[count++] = SPA_DICT_ITEM_INIT(PW_KEY_TARGET_OBJECT, device_id_or_null);
    return count;
}

struct StreamLibPipeWireCaptureStream *streamlib_pipewire_capture_stream_open(
    const void *const *entry_points, const char *device_id_or_null,
    struct StreamLibPipeWireNegotiatedCaptureFormat *negotiated_format_out, char *failure_text,
    size_t failure_text_capacity)
{
    struct StreamLibPipeWireCaptureStream *capture_stream = calloc(1, sizeof(*capture_stream));
    if (capture_stream == NULL) {
        copy_failure_text(failure_text, failure_text_capacity,
                          "out of memory opening a PipeWire capture stream");
        return NULL;
    }
    copy_entry_points(&capture_stream->entry_points, entry_points);
    const struct StreamLibPipeWireEntryPoints *resolved = &capture_stream->entry_points;

    capture_stream->thread_loop = resolved->pw_thread_loop_new("streamlib-audio-capture", NULL);
    if (capture_stream->thread_loop == NULL) {
        copy_failure_text(failure_text, failure_text_capacity,
                          "PipeWire would not create a capture thread loop");
        free(capture_stream);
        return NULL;
    }

    uint8_t pod_storage[STREAMLIB_PIPEWIRE_POD_BUILDER_CAPACITY];
    struct spa_pod_builder pod_builder =
        SPA_POD_BUILDER_INIT(pod_storage, sizeof(pod_storage));
    struct spa_audio_info_raw requested_format = {
        .format = STREAMLIB_PIPEWIRE_REQUESTED_SAMPLE_FORMAT,
    };
    const struct spa_pod *params[1];
    params[0] = spa_format_audio_raw_build(&pod_builder, SPA_PARAM_EnumFormat, &requested_format);

    struct spa_dict_item property_items[4];
    uint32_t property_count = capture_stream_properties(property_items, device_id_or_null);
    struct spa_dict properties = SPA_DICT_INIT(property_items, property_count);

    resolved->pw_thread_loop_lock(capture_stream->thread_loop);
    if (resolved->pw_thread_loop_start(capture_stream->thread_loop) < 0) {
        resolved->pw_thread_loop_unlock(capture_stream->thread_loop);
        copy_failure_text(failure_text, failure_text_capacity,
                          "PipeWire's capture thread loop would not start");
        resolved->pw_thread_loop_destroy(capture_stream->thread_loop);
        free(capture_stream);
        return NULL;
    }

    // `pw_stream_new_simple` takes ownership of the properties, including when
    // it fails, so there is no path here that has to free them.
    struct pw_properties *stream_properties = resolved->pw_properties_new_dict(&properties);
    capture_stream->stream = resolved->pw_stream_new_simple(
        resolved->pw_thread_loop_get_loop(capture_stream->thread_loop), "streamlib-capture",
        stream_properties, &kCaptureStreamEvents, capture_stream);
    if (capture_stream->stream == NULL) {
        resolved->pw_thread_loop_unlock(capture_stream->thread_loop);
        copy_failure_text(failure_text, failure_text_capacity,
                          "PipeWire would not create a capture stream");
        destroy_capture_stream(capture_stream);
        return NULL;
    }

    int connect_result = resolved->pw_stream_connect(
        capture_stream->stream, PW_DIRECTION_INPUT, PW_ID_ANY,
        PW_STREAM_FLAG_AUTOCONNECT | PW_STREAM_FLAG_MAP_BUFFERS | PW_STREAM_FLAG_RT_PROCESS,
        params, 1);
    if (connect_result < 0) {
        resolved->pw_thread_loop_unlock(capture_stream->thread_loop);
        char text[192];
        snprintf(text, sizeof(text), "PipeWire refused the capture connection (%d)",
                 connect_result);
        copy_failure_text(failure_text, failure_text_capacity, text);
        destroy_capture_stream(capture_stream);
        return NULL;
    }

    // Negotiation is what makes the stream's format knowable, and a caller
    // cannot size a block without it — so `open` is where the wait belongs
    // rather than the first callback.
    while (!capture_stream->format_was_negotiated && !capture_stream->stream_failed) {
        if (resolved->pw_thread_loop_timed_wait(capture_stream->thread_loop,
                                                STREAMLIB_PIPEWIRE_NEGOTIATION_TIMEOUT_SECONDS) !=
            0) {
            capture_stream->stream_failed = true;
            snprintf(capture_stream->stream_failure_text,
                     sizeof(capture_stream->stream_failure_text),
                     "PipeWire settled no capture format within %d seconds",
                     STREAMLIB_PIPEWIRE_NEGOTIATION_TIMEOUT_SECONDS);
        }
    }
    resolved->pw_thread_loop_unlock(capture_stream->thread_loop);

    if (capture_stream->stream_failed) {
        copy_failure_text(failure_text, failure_text_capacity,
                          capture_stream->stream_failure_text);
        destroy_capture_stream(capture_stream);
        return NULL;
    }

    if (negotiated_format_out != NULL)
        *negotiated_format_out = capture_stream->negotiated_format;
    return capture_stream;
}

void streamlib_pipewire_capture_stream_start_delivering(
    struct StreamLibPipeWireCaptureStream *capture_stream,
    StreamLibPipeWireCapturedBlockHandOff hand_off, void *hand_off_context)
{
    // Under the loop lock because the process callback reads both fields, and
    // a hand-off paired with the previous caller's context is a use-after-free
    // rather than a lost block.
    capture_stream->entry_points.pw_thread_loop_lock(capture_stream->thread_loop);
    capture_stream->hand_off_context = hand_off_context;
    capture_stream->hand_off = hand_off;
    capture_stream->entry_points.pw_thread_loop_unlock(capture_stream->thread_loop);
}

void streamlib_pipewire_capture_stream_stop_delivering(
    struct StreamLibPipeWireCaptureStream *capture_stream)
{
    // Taking the lock is what makes "the hand-off is not called again once
    // this returns" true: an in-flight callback holds it, so this blocks until
    // that callback has finished.
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

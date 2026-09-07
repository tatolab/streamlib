// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#include "pipewire_audio_shim.h"

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

/// The device's own sample rate and channel count are what a stream settles on
/// in either direction: rate and channel conversion belong to the read-side
/// window stage at a consuming port, which converts to what that port declared,
/// so asking PipeWire to convert either one would convert toward nothing
/// declared. Only the scalar encoding is pinned, and it is pinned little-endian
/// rather than host-endian because `AudioBlock.samples` is little-endian by
/// wire contract.
#define STREAMLIB_PIPEWIRE_REQUESTED_SAMPLE_FORMAT SPA_AUDIO_FORMAT_F32_LE

/// How long `open` waits for PipeWire to settle a format before giving up. A
/// session that has not answered in this long is not going to, and the caller
/// has another arm to demote to.
#define STREAMLIB_PIPEWIRE_NEGOTIATION_TIMEOUT_SECONDS 5

#define STREAMLIB_PIPEWIRE_POD_BUILDER_CAPACITY 1024

struct StreamLibPipeWireAudioStream {
    struct StreamLibPipeWireEntryPoints entry_points;
    struct pw_thread_loop *thread_loop;
    struct pw_stream *stream;
    enum StreamLibPipeWireStreamDirection direction;

    /// Filled by the format callback on the loop thread, read by `open` once
    /// negotiation has been signalled.
    struct StreamLibPipeWireNegotiatedAudioFormat negotiated_format;
    bool format_was_negotiated;
    /// Set when the stream reaches `PW_STREAM_STATE_ERROR`, so a caller
    /// waiting for a format stops waiting rather than sitting out the timeout.
    bool stream_failed;
    char stream_failure_text[256];

    /// NULL until a caller starts handing off; exactly one of the two is ever
    /// set, and which one is the stream's direction.
    ///
    /// Read and written only with the thread loop's lock held. That holds for
    /// the `process` callback because this stream deliberately does not set
    /// `PW_STREAM_FLAG_RT_PROCESS` — see `connect_audio_stream`.
    StreamLibPipeWireCapturedBlockHandOff captured_block_hand_off;
    StreamLibPipeWirePlaybackBlockHandOff playback_block_hand_off;
    void *hand_off_context;

    /// Where a failure goes once a caller holds the stream. NULL until one is
    /// installed, and read and written only with the thread loop's lock held,
    /// like the two above.
    StreamLibPipeWireStreamFailureHandOff stream_failure_hand_off;
    void *stream_failure_hand_off_context;
};

/// How a direction is spelled in text a reader has to act on.
static const char *stream_direction_word(enum StreamLibPipeWireStreamDirection direction)
{
    return direction == STREAMLIB_PIPEWIRE_STREAM_DIRECTION_PLAYBACK ? "playback" : "capture";
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

/// End the stream with a reason, wake whoever is waiting on negotiation, and
/// tell whoever asked to be told.
///
/// The first reason is the one kept. A stream on its way down reports more than
/// once — what broke, then what the teardown behind it made of that — and the
/// first is the one naming the cause.
static void fail_the_stream(struct StreamLibPipeWireAudioStream *audio_stream,
                            const char *reason)
{
    if (audio_stream->stream_failed)
        return;
    audio_stream->stream_failed = true;
    snprintf(audio_stream->stream_failure_text, sizeof(audio_stream->stream_failure_text), "%s",
             reason);
    audio_stream->entry_points.pw_thread_loop_signal(audio_stream->thread_loop, false);
    if (audio_stream->stream_failure_hand_off != NULL) {
        audio_stream->stream_failure_hand_off(audio_stream->stream_failure_hand_off_context,
                                              audio_stream->stream_failure_text);
    }
}

static void on_stream_state_changed(void *data, enum pw_stream_state old_state,
                                    enum pw_stream_state state, const char *error)
{
    struct StreamLibPipeWireAudioStream *audio_stream = data;
    (void)old_state;
    if (state != PW_STREAM_STATE_ERROR)
        return;
    fail_the_stream(audio_stream, error != NULL ? error : "the stream entered its error state");
}

static void on_stream_param_changed(void *data, uint32_t id, const struct spa_pod *param)
{
    struct StreamLibPipeWireAudioStream *audio_stream = data;
    struct spa_audio_info audio_info;
    char reason[sizeof(audio_stream->stream_failure_text)];
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
        fail_the_stream(audio_stream, reason);
        return;
    }
    if (audio_info.info.raw.rate == 0 || audio_info.info.raw.channels == 0) {
        snprintf(reason, sizeof(reason),
                 "PipeWire negotiated %u Hz and %u channels, and no block duration derives "
                 "from either being zero",
                 audio_info.info.raw.rate, audio_info.info.raw.channels);
        fail_the_stream(audio_stream, reason);
        return;
    }

    // A renegotiation after `open` returned would leave the caller framing
    // blocks by a rate and channel count nothing told it about — mis-sized
    // samples and mis-timed stamps rather than a failure. The seam states the
    // format is fixed for the stream's lifetime, so a change ends the stream
    // instead of quietly moving underneath it.
    if (audio_stream->format_was_negotiated) {
        if (audio_stream->negotiated_format.sample_rate != audio_info.info.raw.rate ||
            audio_stream->negotiated_format.channels != audio_info.info.raw.channels ||
            audio_stream->negotiated_format.sample_format != sample_format) {
            snprintf(reason, sizeof(reason),
                     "PipeWire renegotiated this %s stream from %u Hz / %u channels / "
                     "dtype %u to %u Hz / %u channels / dtype %u, and a stream's format is "
                     "fixed for its lifetime",
                     stream_direction_word(audio_stream->direction),
                     audio_stream->negotiated_format.sample_rate,
                     audio_stream->negotiated_format.channels,
                     audio_stream->negotiated_format.sample_format, audio_info.info.raw.rate,
                     audio_info.info.raw.channels, sample_format);
            fail_the_stream(audio_stream, reason);
        }
        return;
    }

    audio_stream->negotiated_format.sample_rate = audio_info.info.raw.rate;
    audio_stream->negotiated_format.channels = audio_info.info.raw.channels;
    audio_stream->negotiated_format.sample_format = sample_format;
    audio_stream->format_was_negotiated = true;
    audio_stream->entry_points.pw_thread_loop_signal(audio_stream->thread_loop, false);
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

static void on_capture_stream_process(void *data)
{
    struct StreamLibPipeWireAudioStream *audio_stream = data;
    const struct StreamLibPipeWireEntryPoints *entry_points = &audio_stream->entry_points;
    const struct StreamLibPipeWireNegotiatedAudioFormat *format =
        &audio_stream->negotiated_format;
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
           (buffer = entry_points->pw_stream_dequeue_buffer(audio_stream->stream)) != NULL) {
        payloads[payload_count] = payload_of(buffer, bytes_per_frame);
        samples_in_this_cycle += payloads[payload_count].sample_count;
        payload_count++;
    }

    if (payload_count > 0 && audio_stream->captured_block_hand_off != NULL && samples_in_this_cycle > 0) {
        struct pw_time time;
        memset(&time, 0, sizeof(time));
        entry_points->pw_stream_get_time_n(audio_stream->stream, &time, sizeof(time));

        int64_t next_timestamp_ns = first_sample_timestamp_of(
            &time, (uint32_t)samples_in_this_cycle, format->sample_rate);
        for (uint32_t index = 0; index < payload_count; index++) {
            if (payloads[index].sample_count == 0)
                continue;
            audio_stream->captured_block_hand_off(
                audio_stream->hand_off_context, payloads[index].samples,
                payloads[index].sample_byte_count, payloads[index].sample_count,
                next_timestamp_ns);
            next_timestamp_ns +=
                nanoseconds_occupied_by(payloads[index].sample_count, format->sample_rate);
        }
    }

    for (uint32_t index = 0; index < payload_count; index++)
        entry_points->pw_stream_queue_buffer(audio_stream->stream, payloads[index].buffer);
}

/// How many whole frames a dequeued playback buffer has room for.
///
/// `pw_buffer.requested` is what the daemon says this cycle needs, and it is
/// the value to fill: writing the whole mapping instead queues more audio than
/// the cycle asked for and grows the latency by a buffer every cycle. Zero
/// means the daemon expressed no preference, where the mapping's own size is
/// the answer.
static uint32_t playback_frames_wanted_by(struct pw_buffer *buffer, uint32_t bytes_per_frame)
{
    struct spa_data *data_plane = &buffer->buffer->datas[0];
    uint32_t frames_the_mapping_holds = data_plane->maxsize / bytes_per_frame;

    if (buffer->requested == 0 || buffer->requested > (uint64_t)frames_the_mapping_holds)
        return frames_the_mapping_holds;
    return (uint32_t)buffer->requested;
}

static void on_playback_stream_process(void *data)
{
    struct StreamLibPipeWireAudioStream *audio_stream = data;
    const struct StreamLibPipeWireEntryPoints *entry_points = &audio_stream->entry_points;
    const struct StreamLibPipeWireNegotiatedAudioFormat *format =
        &audio_stream->negotiated_format;
    uint32_t bytes_per_scalar = 0;
    struct pw_buffer *buffer;

    // Zero until the format settles, and a stride of zero divides nothing.
    if (!bytes_per_scalar_of(format->sample_format, &bytes_per_scalar))
        return;
    uint32_t bytes_per_frame = bytes_per_scalar * format->channels;
    if (bytes_per_frame == 0)
        return;

    buffer = entry_points->pw_stream_dequeue_buffer(audio_stream->stream);
    if (buffer == NULL)
        return;

    struct spa_data *data_plane = &buffer->buffer->datas[0];
    if (data_plane->chunk == NULL) {
        entry_points->pw_stream_queue_buffer(audio_stream->stream, buffer);
        return;
    }

    uint32_t sample_count =
        data_plane->data != NULL ? playback_frames_wanted_by(buffer, bytes_per_frame) : 0;
    size_t sample_byte_count = (size_t)sample_count * bytes_per_frame;

    if (sample_count > 0) {
        if (audio_stream->playback_block_hand_off != NULL) {
            audio_stream->playback_block_hand_off(audio_stream->hand_off_context,
                                                  (uint8_t *)data_plane->data, sample_byte_count,
                                                  sample_count);
        } else {
            // No hand-off yet: the buffer is recycled carrying silence rather
            // than whatever the mapping last held, because a caller still
            // wiring itself up must not make the device replay an old period.
            memset(data_plane->data, 0, sample_byte_count);
        }
    }
    // Written whatever the count, including zero. A recycled buffer keeps the
    // chunk the last cycle left on it, so a cycle that produced nothing would
    // otherwise hand the device the previous period's audio a second time.
    data_plane->chunk->offset = 0;
    data_plane->chunk->stride = (int32_t)bytes_per_frame;
    data_plane->chunk->size = (uint32_t)sample_byte_count;

    entry_points->pw_stream_queue_buffer(audio_stream->stream, buffer);
}

// Version 0 rather than `PW_VERSION_STREAM_EVENTS`: it covers every callback
// this arm installs, and declaring only what is implemented is what lets a
// libpipewire older than the vendored headers dispatch against this struct
// safely.
//
// Two tables rather than one `process` that branches: the direction is fixed
// when the stream is created, so the branch would be re-taken every cycle to
// answer a question that cannot change.
static const struct pw_stream_events kCaptureStreamEvents = {
    .version = 0,
    .state_changed = on_stream_state_changed,
    .param_changed = on_stream_param_changed,
    .process = on_capture_stream_process,
};

static const struct pw_stream_events kPlaybackStreamEvents = {
    .version = 0,
    .state_changed = on_stream_state_changed,
    .param_changed = on_stream_param_changed,
    .process = on_playback_stream_process,
};

/// The event table a stream in `direction` dispatches against.
static const struct pw_stream_events *stream_events_for(
    enum StreamLibPipeWireStreamDirection direction)
{
    return direction == STREAMLIB_PIPEWIRE_STREAM_DIRECTION_PLAYBACK ? &kPlaybackStreamEvents
                                                                     : &kCaptureStreamEvents;
}

/// Free everything the stream holds. The caller must not hold the thread loop's
/// lock — this takes it, and stopping the loop joins its thread.
///
/// The one teardown, reached by every failure path in
/// `streamlib_pipewire_audio_stream_open` as well as by close: a hand-rolled
/// subset at one exit is how that exit ends up skipping a step the others take.
/// Tolerates a NULL stream and a loop that was never started.
static void destroy_audio_stream(struct StreamLibPipeWireAudioStream *audio_stream)
{
    if (audio_stream->stream != NULL) {
        audio_stream->entry_points.pw_thread_loop_lock(audio_stream->thread_loop);
        audio_stream->entry_points.pw_stream_disconnect(audio_stream->stream);
        audio_stream->entry_points.pw_stream_destroy(audio_stream->stream);
        audio_stream->stream = NULL;
        audio_stream->entry_points.pw_thread_loop_unlock(audio_stream->thread_loop);
    }
    if (audio_stream->thread_loop != NULL) {
        audio_stream->entry_points.pw_thread_loop_stop(audio_stream->thread_loop);
        audio_stream->entry_points.pw_thread_loop_destroy(audio_stream->thread_loop);
        audio_stream->thread_loop = NULL;
    }
    free(audio_stream);
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

uint32_t streamlib_pipewire_stream_properties(struct StreamLibPipeWireStreamProperty *items,
                                              uint32_t item_capacity,
                                              enum StreamLibPipeWireStreamDirection direction,
                                              const char *device_id_or_null, char *sink_name,
                                              size_t sink_name_capacity)
{
    uint32_t count = 0;
    bool is_playback = direction == STREAMLIB_PIPEWIRE_STREAM_DIRECTION_PLAYBACK;
    // The capacity is checked rather than trusted, because a property added
    // here would otherwise overwrite the caller's stack silently.
    if (item_capacity < STREAMLIB_PIPEWIRE_MAX_STREAM_PROPERTIES)
        return 0;
    items[count++] = (struct StreamLibPipeWireStreamProperty){PW_KEY_MEDIA_TYPE, "Audio"};
    items[count++] = (struct StreamLibPipeWireStreamProperty){
        PW_KEY_MEDIA_CATEGORY, is_playback ? "Playback" : "Capture"};
    items[count++] = (struct StreamLibPipeWireStreamProperty){PW_KEY_MEDIA_ROLE, "Production"};
    if (device_id_or_null == NULL)
        return count;

    // A sink's monitor is a *capture* endpoint the session already routes, and
    // `stream.capture.sink` is the only way to reach one: targeting a sink
    // without it attaches to the default source instead, which is silence that
    // looks like success. A playback stream targeting a sink wants the sink
    // itself, so the suffix has no meaning there and the plain target path
    // below is the right one.
    size_t sink_name_length =
        is_playback ? 0
                    : streamlib_pipewire_sink_name_length_of_monitor_device_id(device_id_or_null);
    if (sink_name_length > 0) {
        // The caller refuses an over-long monitor id before reaching here, so
        // this cannot fall through to the plain-target path and quietly capture
        // something else.
        if (sink_name_length >= sink_name_capacity)
            return 0;
        memcpy(sink_name, device_id_or_null, sink_name_length);
        sink_name[sink_name_length] = '\0';
        items[count++] =
            (struct StreamLibPipeWireStreamProperty){PW_KEY_TARGET_OBJECT, sink_name};
        items[count++] =
            (struct StreamLibPipeWireStreamProperty){PW_KEY_STREAM_CAPTURE_SINK, "true"};
        return count;
    }

    items[count++] =
        (struct StreamLibPipeWireStreamProperty){PW_KEY_TARGET_OBJECT, device_id_or_null};
    return count;
}

/// Connect the stream in whichever direction it was opened for.
///
/// `PW_STREAM_FLAG_RT_PROCESS` must stay unset. With it, `process` runs on
/// PipeWire's realtime data thread, which does not hold the thread loop's lock
/// — and that lock is what makes installing and retiring a hand-off safe
/// against a callback that is already running.
///
/// `PW_STREAM_FLAG_DONT_RECONNECT` is what makes a named device authoritative:
/// `PW_STREAM_FLAG_AUTOCONNECT` alone treats a target it cannot resolve as
/// licence to link the session default instead, and reaching a device other
/// than the one the caller named is worse than failing. Nothing is set when no
/// device was named — there the session default is what was asked for.
static int connect_audio_stream(struct StreamLibPipeWireAudioStream *audio_stream,
                                const char *device_id_or_null, const struct spa_pod **params,
                                uint32_t param_count)
{
    enum pw_stream_flags flags = PW_STREAM_FLAG_AUTOCONNECT | PW_STREAM_FLAG_MAP_BUFFERS;
    enum pw_direction pw_direction =
        audio_stream->direction == STREAMLIB_PIPEWIRE_STREAM_DIRECTION_PLAYBACK ? PW_DIRECTION_OUTPUT
                                                                                : PW_DIRECTION_INPUT;
    if (device_id_or_null != NULL)
        flags |= PW_STREAM_FLAG_DONT_RECONNECT;
    return audio_stream->entry_points.pw_stream_connect(audio_stream->stream, pw_direction,
                                                        PW_ID_ANY, flags, params, param_count);
}

struct StreamLibPipeWireAudioStream *streamlib_pipewire_audio_stream_open(
    const void *const *entry_points, enum StreamLibPipeWireStreamDirection direction,
    const char *device_id_or_null,
    struct StreamLibPipeWireNegotiatedAudioFormat *negotiated_format_out, char *failure_text,
    size_t failure_text_capacity)
{
    const char *direction_word = stream_direction_word(direction);
    const char *thread_loop_name =
        direction == STREAMLIB_PIPEWIRE_STREAM_DIRECTION_PLAYBACK ? "streamlib-audio-playback"
                                                                  : "streamlib-audio-capture";
    const char *stream_name =
        direction == STREAMLIB_PIPEWIRE_STREAM_DIRECTION_PLAYBACK ? "streamlib-playback"
                                                                  : "streamlib-capture";
    char open_failure_reason[192];
    uint8_t pod_storage[STREAMLIB_PIPEWIRE_POD_BUILDER_CAPACITY];
    struct spa_pod_builder pod_builder = SPA_POD_BUILDER_INIT(pod_storage, sizeof(pod_storage));
    struct spa_audio_info_raw requested_format = {
        .format = STREAMLIB_PIPEWIRE_REQUESTED_SAMPLE_FORMAT,
    };
    struct StreamLibPipeWireStreamProperty composed_properties[
        STREAMLIB_PIPEWIRE_MAX_STREAM_PROPERTIES];
    struct spa_dict_item property_items[STREAMLIB_PIPEWIRE_MAX_STREAM_PROPERTIES];
    // Outlives the dict, which borrows it rather than copying.
    char monitored_sink_name[STREAMLIB_PIPEWIRE_MAX_MONITORED_SINK_NAME_BYTES];
    char over_long_monitor_reason[192];
    uint32_t property_count;
    const struct spa_pod *params[1];
    const struct StreamLibPipeWireEntryPoints *resolved;
    struct pw_properties *stream_properties;
    struct spa_dict properties;
    bool thread_loop_is_locked = false;
    const char *failure_reason;
    char connect_failure_reason[128];
    int connect_result;

    struct StreamLibPipeWireAudioStream *audio_stream = calloc(1, sizeof(*audio_stream));
    if (audio_stream == NULL) {
        snprintf(open_failure_reason, sizeof(open_failure_reason),
                 "out of memory opening a PipeWire %s stream", direction_word);
        streamlib_pipewire_copy_failure_text(failure_text, failure_text_capacity, open_failure_reason);
        return NULL;
    }
    streamlib_pipewire_copy_entry_points(&audio_stream->entry_points, entry_points);
    audio_stream->direction = direction;
    resolved = &audio_stream->entry_points;

    audio_stream->thread_loop = resolved->pw_thread_loop_new(thread_loop_name, NULL);
    if (audio_stream->thread_loop == NULL) {
        snprintf(open_failure_reason, sizeof(open_failure_reason),
                 "PipeWire would not create a %s thread loop", direction_word);
        failure_reason = open_failure_reason;
        goto fail;
    }

    params[0] = spa_format_audio_raw_build(&pod_builder, SPA_PARAM_EnumFormat, &requested_format);

    property_count = streamlib_pipewire_stream_properties(
        composed_properties, STREAMLIB_PIPEWIRE_MAX_STREAM_PROPERTIES, direction,
        device_id_or_null, monitored_sink_name, sizeof(monitored_sink_name));
    if (property_count == 0) {
        // The only way to compose nothing is a monitor id whose sink name will
        // not fit. Named rather than demoted to a plain target: that would ask
        // PipeWire for a source of that name and land on the default one.
        snprintf(over_long_monitor_reason, sizeof(over_long_monitor_reason),
                 "the sink named by this monitor device id is longer than the %d bytes a "
                 "sink name may occupy",
                 STREAMLIB_PIPEWIRE_MAX_MONITORED_SINK_NAME_BYTES - 1);
        failure_reason = over_long_monitor_reason;
        goto fail;
    }
    for (uint32_t index = 0; index < property_count; index++) {
        property_items[index] = SPA_DICT_ITEM_INIT(composed_properties[index].key,
                                                   composed_properties[index].value);
    }
    properties = (struct spa_dict)SPA_DICT_INIT(property_items, property_count);

    resolved->pw_thread_loop_lock(audio_stream->thread_loop);
    thread_loop_is_locked = true;

    if (resolved->pw_thread_loop_start(audio_stream->thread_loop) < 0) {
        snprintf(open_failure_reason, sizeof(open_failure_reason),
                 "PipeWire's %s thread loop would not start", direction_word);
        failure_reason = open_failure_reason;
        goto fail;
    }

    // `pw_stream_new_simple` takes ownership of the properties, including when
    // it fails, so there is no path here that has to free them.
    stream_properties = resolved->pw_properties_new_dict(&properties);
    audio_stream->stream = resolved->pw_stream_new_simple(
        resolved->pw_thread_loop_get_loop(audio_stream->thread_loop), stream_name,
        stream_properties, stream_events_for(direction), audio_stream);
    if (audio_stream->stream == NULL) {
        snprintf(open_failure_reason, sizeof(open_failure_reason),
                 "PipeWire would not create a %s stream", direction_word);
        failure_reason = open_failure_reason;
        goto fail;
    }

    connect_result = connect_audio_stream(audio_stream, device_id_or_null, params, 1);
    if (connect_result < 0) {
        snprintf(connect_failure_reason, sizeof(connect_failure_reason),
                 "PipeWire refused the %s connection (%d)", direction_word, connect_result);
        failure_reason = connect_failure_reason;
        goto fail;
    }

    // Negotiation is what makes the stream's format knowable, and a caller
    // cannot size a block without it — so `open` is where the wait belongs
    // rather than the first callback.
    while (!audio_stream->format_was_negotiated && !audio_stream->stream_failed) {
        if (resolved->pw_thread_loop_timed_wait(audio_stream->thread_loop,
                                                STREAMLIB_PIPEWIRE_NEGOTIATION_TIMEOUT_SECONDS) !=
            0) {
            snprintf(audio_stream->stream_failure_text,
                     sizeof(audio_stream->stream_failure_text),
                     "PipeWire settled no %s format within %d seconds%s", direction_word,
                     STREAMLIB_PIPEWIRE_NEGOTIATION_TIMEOUT_SECONDS,
                     device_id_or_null != NULL
                         ? ", which is what a device id naming no node in the session graph "
                           "looks like"
                         : "");
            audio_stream->stream_failed = true;
        }
    }
    if (audio_stream->stream_failed) {
        failure_reason = audio_stream->stream_failure_text;
        goto fail;
    }

    resolved->pw_thread_loop_unlock(audio_stream->thread_loop);

    if (negotiated_format_out != NULL)
        *negotiated_format_out = audio_stream->negotiated_format;
    return audio_stream;

fail:
    // Copied before the teardown, because the reason may live in the struct the
    // teardown is about to free.
    streamlib_pipewire_copy_failure_text(failure_text, failure_text_capacity, failure_reason);
    if (thread_loop_is_locked)
        resolved->pw_thread_loop_unlock(audio_stream->thread_loop);
    destroy_audio_stream(audio_stream);
    return NULL;
}

/// Swap in a hand-off pair under the thread loop's lock.
///
/// The lock is what makes the pair atomic with respect to `process`, which
/// reads both fields holding that same lock: a hand-off paired with the
/// previous caller's context is a use-after-free rather than a lost block.
/// Exactly one of the two pointers is ever non-NULL — the stream's direction
/// decides which — so this also retires whichever one an earlier call left.
static void install_hand_off(struct StreamLibPipeWireAudioStream *audio_stream,
                             StreamLibPipeWireCapturedBlockHandOff captured_block_hand_off,
                             StreamLibPipeWirePlaybackBlockHandOff playback_block_hand_off,
                             void *hand_off_context)
{
    audio_stream->entry_points.pw_thread_loop_lock(audio_stream->thread_loop);
    audio_stream->hand_off_context = hand_off_context;
    audio_stream->captured_block_hand_off = captured_block_hand_off;
    audio_stream->playback_block_hand_off = playback_block_hand_off;
    audio_stream->entry_points.pw_thread_loop_unlock(audio_stream->thread_loop);
}

void streamlib_pipewire_capture_stream_start_delivering(
    struct StreamLibPipeWireAudioStream *audio_stream,
    StreamLibPipeWireCapturedBlockHandOff hand_off, void *hand_off_context)
{
    install_hand_off(audio_stream, hand_off, NULL, hand_off_context);
}

void streamlib_pipewire_playback_stream_start_requesting(
    struct StreamLibPipeWireAudioStream *audio_stream,
    StreamLibPipeWirePlaybackBlockHandOff hand_off, void *hand_off_context)
{
    install_hand_off(audio_stream, NULL, hand_off, hand_off_context);
}

void streamlib_pipewire_audio_stream_report_failures_to(
    struct StreamLibPipeWireAudioStream *audio_stream,
    StreamLibPipeWireStreamFailureHandOff hand_off, void *hand_off_context)
{
    audio_stream->entry_points.pw_thread_loop_lock(audio_stream->thread_loop);
    audio_stream->stream_failure_hand_off_context = hand_off_context;
    audio_stream->stream_failure_hand_off = hand_off;
    // A stream that failed between opening and this call recorded its reason
    // with nobody installed to hear it, so it is handed over now instead. The
    // caller's answer must not depend on how fast it wired itself up.
    if (hand_off != NULL && audio_stream->stream_failed)
        hand_off(hand_off_context, audio_stream->stream_failure_text);
    audio_stream->entry_points.pw_thread_loop_unlock(audio_stream->thread_loop);
}

void streamlib_pipewire_audio_stream_stop_handing_off(
    struct StreamLibPipeWireAudioStream *audio_stream)
{
    // Taking the lock is what makes "the hand-off is not called again once this
    // returns" true: `process` runs on the loop thread holding this same lock,
    // so an in-flight callback owns it and this blocks until that callback has
    // finished. It is why `PW_STREAM_FLAG_RT_PROCESS` is not set — see
    // `connect_audio_stream`.
    install_hand_off(audio_stream, NULL, NULL, NULL);
}

void streamlib_pipewire_audio_stream_close(struct StreamLibPipeWireAudioStream *audio_stream)
{
    if (audio_stream == NULL)
        return;
    streamlib_pipewire_audio_stream_stop_handing_off(audio_stream);
    // Retired under the loop lock for the same reason the sample hand-offs are:
    // the caller frees what the context points at once this returns.
    streamlib_pipewire_audio_stream_report_failures_to(audio_stream, NULL, NULL);
    destroy_audio_stream(audio_stream);
}

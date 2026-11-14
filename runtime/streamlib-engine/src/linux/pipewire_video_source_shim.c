// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

#include "pipewire_video_source_shim.h"

#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#include <pipewire/keys.h>
#include <pipewire/properties.h>
#include <pipewire/stream.h>
#include <pipewire/thread-loop.h>
#include <spa/buffer/buffer.h>
#include <spa/buffer/meta.h>
#include <spa/param/buffers.h>
#include <spa/param/param.h>
#include <spa/param/video/format-utils.h>
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
// `failure_text` buffer or the source's own, and the Rust side emits them
// through `tracing`.

/// Packed 4:2:2, two bytes a pixel: what the loopback door writes, so both
/// doors carry the one picture `convert_surface_into` produces.
#define STREAMLIB_PIPEWIRE_VIDEO_FORMAT SPA_VIDEO_FORMAT_YUY2
#define STREAMLIB_PIPEWIRE_VIDEO_BYTES_PER_PIXEL 2

#define STREAMLIB_PIPEWIRE_VIDEO_POD_BUILDER_CAPACITY 2048

/// What `node.name` is prefixed with, so every StreamLib camera is one grep
/// away in `pw-dump` whatever the user called it.
#define STREAMLIB_PIPEWIRE_VIDEO_NODE_NAME_PREFIX "streamlib-camera-"

/// One negotiated buffer, in two lifetimes.
///
/// The first two live with PipeWire's buffer set — added and removed as
/// consumers come and go. The shared-memory allocation lives with the offered
/// extent instead, so a consumer reconnecting reuses it rather than the shim
/// mapping and unmapping under every reconnect.
struct StreamLibPipeWireVideoBufferSlot {
    struct pw_buffer *pipewire_buffer;
    /// Held between dequeue and queue, and NULL otherwise.
    struct pw_buffer *dequeued_buffer;
    /// -1 and NULL until a consumer takes this slot, and then stable until the
    /// extent is replaced.
    int shared_memory_fd;
    uint8_t *shared_memory;
    size_t shared_memory_byte_size;
};

struct StreamLibPipeWireVideoSource {
    struct StreamLibPipeWireEntryPoints entry_points;
    struct pw_thread_loop *thread_loop;
    struct pw_stream *stream;

    /// Scratch the property dict borrows for the stream's whole life.
    char node_name[STREAMLIB_PIPEWIRE_VIDEO_NODE_NAME_CAPACITY];

    /// The picture last offered and its buffer count. Read and written only
    /// with the thread loop's lock held.
    struct StreamLibPipeWireVideoOfferedFormat offered_format;
    uint32_t buffer_count;

    bool consumer_negotiated;
    struct StreamLibPipeWireVideoBufferSlot slots[STREAMLIB_PIPEWIRE_VIDEO_MAX_BUFFERS];

    bool stream_failed;
    char stream_failure_text[256];
};

/// Record a failure a caller will read back later, without overwriting the
/// first one: the first thing that went wrong explains the rest.
static void record_stream_failure(struct StreamLibPipeWireVideoSource *video_source,
                                  const char *reason)
{
    if (video_source->stream_failed)
        return;
    video_source->stream_failed = true;
    snprintf(video_source->stream_failure_text, sizeof(video_source->stream_failure_text), "%s",
             reason);
}

size_t streamlib_pipewire_video_source_node_name(const char *camera_name, char *node_name,
                                                 size_t node_name_capacity)
{
    const size_t prefix_length = strlen(STREAMLIB_PIPEWIRE_VIDEO_NODE_NAME_PREFIX);
    if (node_name == NULL || node_name_capacity <= prefix_length + 1)
        return 0;
    memcpy(node_name, STREAMLIB_PIPEWIRE_VIDEO_NODE_NAME_PREFIX, prefix_length);

    size_t written = prefix_length;
    bool previous_was_separator = true;
    for (const char *cursor = camera_name != NULL ? camera_name : ""; *cursor != '\0'; cursor++) {
        if (written + 1 >= node_name_capacity)
            break;
        unsigned char character = (unsigned char)*cursor;
        bool is_identifier_character = (character >= '0' && character <= '9') ||
                                       (character >= 'a' && character <= 'z') ||
                                       (character >= 'A' && character <= 'Z');
        if (!is_identifier_character) {
            // Runs collapse rather than repeat, so "Desk  cam!" and "Desk-cam"
            // reach the same identifier instead of two that differ by padding.
            if (previous_was_separator)
                continue;
            node_name[written++] = '-';
            previous_was_separator = true;
            continue;
        }
        if (character >= 'A' && character <= 'Z')
            character = (unsigned char)(character - 'A' + 'a');
        node_name[written++] = (char)character;
        previous_was_separator = false;
    }
    // A name that was only separators leaves trailing ones behind, and the
    // floor is one below the prefix so that its own joining dash goes too:
    // an unnameable camera registers as `streamlib-camera`, not as a name
    // ending in a dangling separator.
    while (written > prefix_length - 1 && node_name[written - 1] == '-')
        written--;
    node_name[written] = '\0';
    return written;
}

uint32_t streamlib_pipewire_video_source_properties(struct StreamLibPipeWireStreamProperty *items,
                                                    uint32_t item_capacity,
                                                    const char *camera_name, char *node_name,
                                                    size_t node_name_capacity)
{
    // The capacity is checked rather than trusted, because a property added
    // here would otherwise overwrite the caller's stack silently.
    if (item_capacity < STREAMLIB_PIPEWIRE_VIDEO_SOURCE_PROPERTY_COUNT || camera_name == NULL)
        return 0;
    if (streamlib_pipewire_video_source_node_name(camera_name, node_name, node_name_capacity) == 0)
        return 0;

    uint32_t count = 0;
    items[count++] = (struct StreamLibPipeWireStreamProperty){PW_KEY_MEDIA_CLASS, "Video/Source"};
    items[count++] = (struct StreamLibPipeWireStreamProperty){PW_KEY_MEDIA_ROLE, "Camera"};
    items[count++] = (struct StreamLibPipeWireStreamProperty){PW_KEY_MEDIA_TYPE, "Video"};
    items[count++] = (struct StreamLibPipeWireStreamProperty){PW_KEY_MEDIA_CATEGORY, "Capture"};
    items[count++] = (struct StreamLibPipeWireStreamProperty){PW_KEY_NODE_NAME, node_name};
    items[count++] = (struct StreamLibPipeWireStreamProperty){PW_KEY_NODE_DESCRIPTION, camera_name};
    return count;
}

/// The one `EnumFormat` a camera offers: raw YUY2 with no modifier property,
/// which is what makes it a shared-memory offer.
static struct spa_pod *build_shared_memory_format(
    struct spa_pod_builder *pod_builder,
    const struct StreamLibPipeWireVideoOfferedFormat *offered_format)
{
    struct spa_video_info_raw info = {
        .format = STREAMLIB_PIPEWIRE_VIDEO_FORMAT,
        .size = SPA_RECTANGLE(offered_format->width, offered_format->height),
        .framerate = SPA_FRACTION(offered_format->framerate_numerator,
                                  offered_format->framerate_denominator),
    };
    return spa_format_video_raw_build(pod_builder, SPA_PARAM_EnumFormat, &info);
}

uint32_t streamlib_pipewire_video_source_offered_video_format(void)
{
    return STREAMLIB_PIPEWIRE_VIDEO_FORMAT;
}

struct StreamLibPipeWireVideoOfferReport streamlib_pipewire_video_source_describe_offer(
    const struct StreamLibPipeWireVideoOfferedFormat *offered_format)
{
    uint8_t pod_storage[STREAMLIB_PIPEWIRE_VIDEO_POD_BUILDER_CAPACITY];
    struct spa_pod_builder pod_builder = SPA_POD_BUILDER_INIT(pod_storage, sizeof(pod_storage));
    struct StreamLibPipeWireVideoOfferReport report = {0};

    struct spa_pod *format = build_shared_memory_format(&pod_builder, offered_format);
    if (format == NULL)
        return report;
    struct spa_video_info_raw parsed = {0};
    if (spa_format_video_raw_parse(format, &parsed) < 0)
        return report;
    report.width = parsed.size.width;
    report.height = parsed.size.height;
    report.video_format = parsed.format;
    report.carries_a_modifier = spa_pod_find_prop(format, NULL, SPA_FORMAT_VIDEO_modifier) != NULL;
    report.was_built = true;
    return report;
}

/// Bytes one row of the offered picture occupies.
static uint32_t offered_stride_bytes(const struct StreamLibPipeWireVideoSource *video_source)
{
    return video_source->offered_format.width * STREAMLIB_PIPEWIRE_VIDEO_BYTES_PER_PIXEL;
}

/// Bytes one whole picture occupies.
static uint32_t offered_byte_size(const struct StreamLibPipeWireVideoSource *video_source)
{
    return offered_stride_bytes(video_source) * video_source->offered_format.height;
}

/// Give up one buffer of PipeWire's set, leaving the extent's shared memory
/// where it is.
static void release_slot_buffer(struct StreamLibPipeWireVideoBufferSlot *slot)
{
    slot->pipewire_buffer = NULL;
    slot->dequeued_buffer = NULL;
}

static void release_every_slot_buffer(struct StreamLibPipeWireVideoSource *video_source)
{
    for (uint32_t index = 0; index < STREAMLIB_PIPEWIRE_VIDEO_MAX_BUFFERS; index++)
        release_slot_buffer(&video_source->slots[index]);
}

/// Give up the extent's shared memory, on a replaced extent or a closing
/// source.
static void release_every_slot_shared_memory(struct StreamLibPipeWireVideoSource *video_source)
{
    for (uint32_t index = 0; index < STREAMLIB_PIPEWIRE_VIDEO_MAX_BUFFERS; index++) {
        struct StreamLibPipeWireVideoBufferSlot *slot = &video_source->slots[index];
        if (slot->shared_memory != NULL) {
            munmap(slot->shared_memory, slot->shared_memory_byte_size);
            slot->shared_memory = NULL;
        }
        if (slot->shared_memory_fd >= 0) {
            close(slot->shared_memory_fd);
            slot->shared_memory_fd = -1;
        }
        slot->shared_memory_byte_size = 0;
    }
}

/// Answer a settled format with the buffer and metadata parameters, which is
/// what makes PipeWire allocate.
static void on_stream_param_changed(void *data, uint32_t id, const struct spa_pod *param)
{
    struct StreamLibPipeWireVideoSource *video_source = data;
    const struct StreamLibPipeWireEntryPoints *entry_points = &video_source->entry_points;
    uint8_t pod_storage[STREAMLIB_PIPEWIRE_VIDEO_POD_BUILDER_CAPACITY];
    struct spa_pod_builder pod_builder = SPA_POD_BUILDER_INIT(pod_storage, sizeof(pod_storage));

    if (id != SPA_PARAM_Format)
        return;
    if (param == NULL) {
        // The consumer went away and the format was cleared; the next one
        // negotiates from scratch.
        video_source->consumer_negotiated = false;
        return;
    }

    uint32_t media_type = 0;
    uint32_t media_subtype = 0;
    if (spa_format_parse(param, &media_type, &media_subtype) < 0 ||
        media_type != SPA_MEDIA_TYPE_video || media_subtype != SPA_MEDIA_SUBTYPE_raw) {
        record_stream_failure(video_source, "PipeWire settled a format that is not raw video");
        return;
    }
    struct spa_video_info_raw negotiated = {0};
    if (spa_format_video_raw_parse(param, &negotiated) < 0) {
        record_stream_failure(video_source, "PipeWire settled a raw video format that would "
                                            "not parse");
        return;
    }
    video_source->consumer_negotiated = true;

    const struct spa_pod *params[2];
    params[0] = spa_pod_builder_add_object(
        &pod_builder, SPA_TYPE_OBJECT_ParamBuffers, SPA_PARAM_Buffers, SPA_PARAM_BUFFERS_buffers,
        SPA_POD_CHOICE_RANGE_Int((int32_t)video_source->buffer_count, 1,
                                 STREAMLIB_PIPEWIRE_VIDEO_MAX_BUFFERS),
        SPA_PARAM_BUFFERS_blocks,
        SPA_POD_Int(1), SPA_PARAM_BUFFERS_size,
        SPA_POD_Int((int32_t)offered_byte_size(video_source)), SPA_PARAM_BUFFERS_stride,
        SPA_POD_Int((int32_t)offered_stride_bytes(video_source)), SPA_PARAM_BUFFERS_align,
        SPA_POD_Int(16), SPA_PARAM_BUFFERS_dataType, SPA_POD_Int(1 << SPA_DATA_MemFd));
    params[1] = spa_pod_builder_add_object(
        &pod_builder, SPA_TYPE_OBJECT_ParamMeta, SPA_PARAM_Meta, SPA_PARAM_META_type,
        SPA_POD_Id(SPA_META_Header), SPA_PARAM_META_size,
        SPA_POD_Int((int32_t)sizeof(struct spa_meta_header)));
    entry_points->pw_stream_update_params(video_source->stream, params, 2);
}

/// The slot a newly added buffer takes: the first one PipeWire is not already
/// holding.
///
/// First-free rather than a running count, because `remove_buffer` can retire
/// buffers in any order — a counter would hand the next `add_buffer` a slot
/// still in use.
static int32_t first_free_slot(const struct StreamLibPipeWireVideoSource *video_source)
{
    for (uint32_t index = 0; index < STREAMLIB_PIPEWIRE_VIDEO_MAX_BUFFERS; index++) {
        if (video_source->slots[index].pipewire_buffer == NULL)
            return (int32_t)index;
    }
    return -1;
}

/// Give one allocated PipeWire buffer its memory. `PW_STREAM_FLAG_ALLOC_BUFFERS`
/// makes every block the client's to provide, and a slot's memfd is allocated
/// at most once per offered extent.
static void on_stream_add_buffer(void *data, struct pw_buffer *buffer)
{
    struct StreamLibPipeWireVideoSource *video_source = data;
    struct spa_data *block = &buffer->buffer->datas[0];

    int32_t slot_index = first_free_slot(video_source);
    if (slot_index < 0) {
        record_stream_failure(video_source,
                              "PipeWire allocated more buffers than the camera offered");
        return;
    }
    struct StreamLibPipeWireVideoBufferSlot *slot = &video_source->slots[slot_index];

    if (slot->shared_memory == NULL) {
        size_t byte_size = offered_byte_size(video_source);
        int shared_memory_fd = memfd_create("streamlib-camera", MFD_CLOEXEC);
        if (shared_memory_fd < 0) {
            record_stream_failure(video_source, "the camera could not create a memfd");
            return;
        }
        if (ftruncate(shared_memory_fd, (off_t)byte_size) < 0) {
            close(shared_memory_fd);
            record_stream_failure(video_source, "the camera could not size its memfd");
            return;
        }
        void *mapping =
            mmap(NULL, byte_size, PROT_READ | PROT_WRITE, MAP_SHARED, shared_memory_fd, 0);
        if (mapping == MAP_FAILED) {
            close(shared_memory_fd);
            record_stream_failure(video_source, "the camera could not map its memfd");
            return;
        }
        slot->shared_memory_fd = shared_memory_fd;
        slot->shared_memory = mapping;
        slot->shared_memory_byte_size = byte_size;
    }

    block->type = SPA_DATA_MemFd;
    block->flags = SPA_DATA_FLAG_READABLE;
    block->fd = slot->shared_memory_fd;
    block->mapoffset = 0;
    block->maxsize = (uint32_t)slot->shared_memory_byte_size;
    block->data = slot->shared_memory;
    block->chunk->offset = 0;
    block->chunk->stride = (int32_t)offered_stride_bytes(video_source);
    block->chunk->size = (uint32_t)slot->shared_memory_byte_size;

    slot->pipewire_buffer = buffer;
    // The slot index survives the round trip through PipeWire, so a dequeued
    // buffer names its own mapping.
    buffer->user_data = (void *)(uintptr_t)(slot_index + 1);
}

static void on_stream_remove_buffer(void *data, struct pw_buffer *buffer)
{
    struct StreamLibPipeWireVideoSource *video_source = data;
    for (uint32_t index = 0; index < STREAMLIB_PIPEWIRE_VIDEO_MAX_BUFFERS; index++) {
        if (video_source->slots[index].pipewire_buffer != buffer)
            continue;
        release_slot_buffer(&video_source->slots[index]);
        return;
    }
}

static void on_stream_state_changed(void *data, enum pw_stream_state old_state,
                                    enum pw_stream_state state, const char *error)
{
    struct StreamLibPipeWireVideoSource *video_source = data;
    (void)old_state;
    if (state == PW_STREAM_STATE_ERROR) {
        char reason[256];
        snprintf(reason, sizeof(reason), "the PipeWire camera node failed: %s",
                 error != NULL ? error : "no reason given");
        record_stream_failure(video_source, reason);
    }
    video_source->entry_points.pw_thread_loop_signal(video_source->thread_loop, false);
}

// Version 0 rather than `PW_VERSION_STREAM_EVENTS`: it covers every callback
// this arm installs, and declaring only what is implemented is what lets a
// libpipewire older than the vendored headers dispatch against this struct
// safely.
//
// No `process`: the stream is the driver, so a cycle happens because
// `pw_stream_trigger_process` said so, not because PipeWire asked.
static const struct pw_stream_events kVideoSourceStreamEvents = {
    .version = 0,
    .state_changed = on_stream_state_changed,
    .param_changed = on_stream_param_changed,
    .add_buffer = on_stream_add_buffer,
    .remove_buffer = on_stream_remove_buffer,
};

/// Free everything the source holds. The caller must not hold the thread
/// loop's lock — this takes it, and stopping the loop joins its thread.
static void destroy_video_source(struct StreamLibPipeWireVideoSource *video_source)
{
    if (video_source->stream != NULL) {
        video_source->entry_points.pw_thread_loop_lock(video_source->thread_loop);
        video_source->entry_points.pw_stream_disconnect(video_source->stream);
        video_source->entry_points.pw_stream_destroy(video_source->stream);
        video_source->stream = NULL;
        release_every_slot_buffer(video_source);
        video_source->entry_points.pw_thread_loop_unlock(video_source->thread_loop);
    }
    if (video_source->thread_loop != NULL) {
        video_source->entry_points.pw_thread_loop_stop(video_source->thread_loop);
        video_source->entry_points.pw_thread_loop_destroy(video_source->thread_loop);
        video_source->thread_loop = NULL;
    }
    // The buffers went with the stream above; the extent's shared memory is
    // this source's alone and goes last.
    release_every_slot_shared_memory(video_source);
    free(video_source);
}

struct StreamLibPipeWireVideoSource *streamlib_pipewire_video_source_open(
    const void *const *entry_points, const char *camera_name, char *failure_text,
    size_t failure_text_capacity)
{
    struct StreamLibPipeWireStreamProperty composed_properties
        [STREAMLIB_PIPEWIRE_VIDEO_SOURCE_PROPERTY_COUNT];
    struct spa_dict_item property_items[STREAMLIB_PIPEWIRE_VIDEO_SOURCE_PROPERTY_COUNT];
    const char *failure_reason;
    bool thread_loop_is_locked = false;
    struct pw_properties *stream_properties;
    struct spa_dict properties;
    char connect_failure_reason[128];
    int connect_result;

    struct StreamLibPipeWireVideoSource *video_source = calloc(1, sizeof(*video_source));
    if (video_source == NULL) {
        streamlib_pipewire_copy_failure_text(failure_text, failure_text_capacity,
                          "out of memory opening a PipeWire camera node");
        return NULL;
    }
    streamlib_pipewire_copy_entry_points(&video_source->entry_points, entry_points);
    const struct StreamLibPipeWireEntryPoints *resolved = &video_source->entry_points;
    for (uint32_t index = 0; index < STREAMLIB_PIPEWIRE_VIDEO_MAX_BUFFERS; index++) {
        video_source->slots[index].shared_memory_fd = -1;
    }

    uint32_t property_count = streamlib_pipewire_video_source_properties(
        composed_properties, STREAMLIB_PIPEWIRE_VIDEO_SOURCE_PROPERTY_COUNT, camera_name,
        video_source->node_name, sizeof(video_source->node_name));
    if (property_count == 0) {
        failure_reason = "this camera's name does not reduce to a PipeWire node name";
        goto fail;
    }
    for (uint32_t index = 0; index < property_count; index++) {
        property_items[index] =
            SPA_DICT_ITEM_INIT(composed_properties[index].key, composed_properties[index].value);
    }
    properties = (struct spa_dict)SPA_DICT_INIT(property_items, property_count);

    video_source->thread_loop = resolved->pw_thread_loop_new("streamlib-camera", NULL);
    if (video_source->thread_loop == NULL) {
        failure_reason = "PipeWire would not create a camera thread loop";
        goto fail;
    }

    resolved->pw_thread_loop_lock(video_source->thread_loop);
    thread_loop_is_locked = true;

    if (resolved->pw_thread_loop_start(video_source->thread_loop) < 0) {
        failure_reason = "PipeWire's camera thread loop would not start";
        goto fail;
    }

    // `pw_stream_new_simple` takes ownership of the properties, including when
    // it fails, so there is no path here that has to free them.
    stream_properties = resolved->pw_properties_new_dict(&properties);
    video_source->stream = resolved->pw_stream_new_simple(
        resolved->pw_thread_loop_get_loop(video_source->thread_loop), video_source->node_name,
        stream_properties, &kVideoSourceStreamEvents, video_source);
    if (video_source->stream == NULL) {
        failure_reason = "PipeWire would not create a camera stream";
        goto fail;
    }

    // Connected with no format at all and inactive: the node registers and is
    // listed, and nothing can negotiate against it until `set_extent` states a
    // format.
    //
    // No `AUTOCONNECT`: a camera is selected by whoever wants it, not linked to
    // the session default. `DRIVER` because the frame's arrival is the cadence,
    // and `ALLOC_BUFFERS` because the camera owns its memfds.
    connect_result = resolved->pw_stream_connect(
        video_source->stream, PW_DIRECTION_OUTPUT, PW_ID_ANY,
        PW_STREAM_FLAG_INACTIVE | PW_STREAM_FLAG_DRIVER | PW_STREAM_FLAG_ALLOC_BUFFERS, NULL, 0);
    if (connect_result < 0) {
        snprintf(connect_failure_reason, sizeof(connect_failure_reason),
                 "PipeWire refused to register the camera node (%d)", connect_result);
        failure_reason = connect_failure_reason;
        goto fail;
    }

    resolved->pw_thread_loop_unlock(video_source->thread_loop);
    return video_source;

fail:
    streamlib_pipewire_copy_failure_text(failure_text, failure_text_capacity, failure_reason);
    if (thread_loop_is_locked)
        resolved->pw_thread_loop_unlock(video_source->thread_loop);
    destroy_video_source(video_source);
    return NULL;
}

int streamlib_pipewire_video_source_set_extent(
    struct StreamLibPipeWireVideoSource *video_source, uint32_t width, uint32_t height,
    uint32_t framerate_numerator, uint32_t framerate_denominator, uint32_t buffer_count,
    char *failure_text, size_t failure_text_capacity)
{
    uint8_t pod_storage[STREAMLIB_PIPEWIRE_VIDEO_POD_BUILDER_CAPACITY];
    struct spa_pod_builder pod_builder = SPA_POD_BUILDER_INIT(pod_storage, sizeof(pod_storage));

    if (buffer_count == 0 || buffer_count > STREAMLIB_PIPEWIRE_VIDEO_MAX_BUFFERS) {
        streamlib_pipewire_copy_failure_text(failure_text, failure_text_capacity,
                                             "a camera offers between one and eight buffers");
        return -1;
    }
    if (width == 0 || height == 0 || framerate_denominator == 0) {
        streamlib_pipewire_copy_failure_text(failure_text, failure_text_capacity,
                          "a camera's extent and frame interval must both be non-zero");
        return -1;
    }

    const struct StreamLibPipeWireEntryPoints *resolved = &video_source->entry_points;
    resolved->pw_thread_loop_lock(video_source->thread_loop);

    // The previous extent's shared memory goes here and nowhere else. A
    // consumer that still holds a buffer reads through its own mapping of the
    // memfd, not this address space's.
    release_every_slot_shared_memory(video_source);

    video_source->offered_format = (struct StreamLibPipeWireVideoOfferedFormat){
        .width = width,
        .height = height,
        .framerate_numerator = framerate_numerator,
        .framerate_denominator = framerate_denominator,
    };
    video_source->buffer_count = buffer_count;
    video_source->consumer_negotiated = false;

    const struct spa_pod *params[1] = {
        build_shared_memory_format(&pod_builder, &video_source->offered_format),
    };
    int update_result = resolved->pw_stream_update_params(video_source->stream, params, 1);
    if (update_result >= 0)
        resolved->pw_stream_set_active(video_source->stream, true);
    resolved->pw_thread_loop_unlock(video_source->thread_loop);

    if (update_result < 0) {
        char reason[128];
        snprintf(reason, sizeof(reason), "PipeWire refused the camera's format (%d)",
                 update_result);
        streamlib_pipewire_copy_failure_text(failure_text, failure_text_capacity, reason);
        return -1;
    }
    return 0;
}

bool streamlib_pipewire_video_source_consumer_negotiated(
    struct StreamLibPipeWireVideoSource *video_source)
{
    const struct StreamLibPipeWireEntryPoints *resolved = &video_source->entry_points;
    resolved->pw_thread_loop_lock(video_source->thread_loop);
    bool negotiated = video_source->consumer_negotiated;
    resolved->pw_thread_loop_unlock(video_source->thread_loop);
    return negotiated;
}

/// The slot a dequeued buffer names, or -1 when it carries none.
static int32_t slot_index_of(struct pw_buffer *buffer)
{
    uintptr_t stored = (uintptr_t)buffer->user_data;
    if (stored == 0 || stored > STREAMLIB_PIPEWIRE_VIDEO_MAX_BUFFERS)
        return -1;
    return (int32_t)(stored - 1);
}

int32_t streamlib_pipewire_video_source_dequeue_slot(
    struct StreamLibPipeWireVideoSource *video_source)
{
    const struct StreamLibPipeWireEntryPoints *resolved = &video_source->entry_points;
    resolved->pw_thread_loop_lock(video_source->thread_loop);

    // A slot an earlier frame took and could not fill is still ours, so it is
    // handed out again rather than a fresh one being taken. There is no way to
    // give an output buffer back unpublished — `pw_stream_queue_buffer` submits
    // it — so holding it is the only way a failed frame does not become the
    // previous picture republished under a new consumer's eyes.
    int32_t slot = -1;
    for (uint32_t index = 0; index < STREAMLIB_PIPEWIRE_VIDEO_MAX_BUFFERS; index++) {
        if (video_source->slots[index].dequeued_buffer != NULL) {
            slot = (int32_t)index;
            break;
        }
    }
    if (slot < 0) {
        struct pw_buffer *buffer = resolved->pw_stream_dequeue_buffer(video_source->stream);
        if (buffer != NULL) {
            slot = slot_index_of(buffer);
            if (slot < 0) {
                // A buffer the shim never gave memory to names no picture, and
                // publishing it would hand the consumer whatever the mapping
                // last held. It is kept and the stream is failed by name.
                record_stream_failure(video_source,
                                      "PipeWire handed back a buffer this camera never filled");
            } else {
                video_source->slots[slot].dequeued_buffer = buffer;
            }
        }
    }
    resolved->pw_thread_loop_unlock(video_source->thread_loop);
    return slot;
}

uint8_t *streamlib_pipewire_video_source_slot_shared_memory(
    struct StreamLibPipeWireVideoSource *video_source, int32_t slot, uint32_t *stride_bytes_out,
    uint32_t *byte_size_out)
{
    if (slot < 0 || slot >= STREAMLIB_PIPEWIRE_VIDEO_MAX_BUFFERS)
        return NULL;
    const struct StreamLibPipeWireEntryPoints *resolved = &video_source->entry_points;
    resolved->pw_thread_loop_lock(video_source->thread_loop);
    uint8_t *mapping = video_source->slots[slot].shared_memory;
    if (mapping != NULL) {
        if (stride_bytes_out != NULL)
            *stride_bytes_out = offered_stride_bytes(video_source);
        if (byte_size_out != NULL)
            *byte_size_out = (uint32_t)video_source->slots[slot].shared_memory_byte_size;
    }
    resolved->pw_thread_loop_unlock(video_source->thread_loop);
    return mapping;
}

int streamlib_pipewire_video_source_queue_slot(struct StreamLibPipeWireVideoSource *video_source,
                                               int32_t slot, int64_t timestamp_ns,
                                               uint64_t sequence)
{
    if (slot < 0 || slot >= STREAMLIB_PIPEWIRE_VIDEO_MAX_BUFFERS)
        return -1;
    const struct StreamLibPipeWireEntryPoints *resolved = &video_source->entry_points;
    resolved->pw_thread_loop_lock(video_source->thread_loop);

    struct pw_buffer *buffer = video_source->slots[slot].dequeued_buffer;
    if (buffer == NULL) {
        resolved->pw_thread_loop_unlock(video_source->thread_loop);
        return -1;
    }
    struct spa_data *block = &buffer->buffer->datas[0];
    if (block->chunk != NULL) {
        // Rewritten every cycle: a recycled buffer keeps the chunk the last one
        // left on it, so a stale size would hand the consumer the previous
        // picture's extent.
        block->chunk->offset = 0;
        block->chunk->stride = (int32_t)offered_stride_bytes(video_source);
        block->chunk->size = block->maxsize;
        block->chunk->flags = 0;
    }

    struct spa_meta_header *header =
        spa_buffer_find_meta_data(buffer->buffer, SPA_META_Header, sizeof(*header));
    if (header == NULL) {
        // The stamp is what a consumer joins this camera to anything else by,
        // and the header meta is the only place it travels. A buffer without
        // one is failed by name rather than published looking fine and
        // carrying no time at all. The slot stays held, so nothing republishes.
        record_stream_failure(video_source,
                              "PipeWire allocated buffers with no header metadata, so a frame's "
                              "timestamp has nowhere to travel");
        resolved->pw_thread_loop_unlock(video_source->thread_loop);
        return -1;
    }
    header->flags = 0;
    header->offset = 0;
    header->seq = sequence;
    header->pts = timestamp_ns;
    header->dts_offset = 0;

    int queue_result = resolved->pw_stream_queue_buffer(video_source->stream, buffer);
    if (queue_result >= 0) {
        // Released only once PipeWire has taken it: a refused queue leaves the
        // buffer under this caller's control, and forgetting it here would
        // drop it out of circulation for the stream's whole life.
        video_source->slots[slot].dequeued_buffer = NULL;
        // The stream is the graph's driver, so nothing runs a cycle until this
        // says one is due.
        resolved->pw_stream_trigger_process(video_source->stream);
    }
    resolved->pw_thread_loop_unlock(video_source->thread_loop);
    return queue_result >= 0 ? 0 : -1;
}

const char *streamlib_pipewire_video_source_failure(
    struct StreamLibPipeWireVideoSource *video_source)
{
    const struct StreamLibPipeWireEntryPoints *resolved = &video_source->entry_points;
    resolved->pw_thread_loop_lock(video_source->thread_loop);
    const char *failure = video_source->stream_failed ? video_source->stream_failure_text : NULL;
    resolved->pw_thread_loop_unlock(video_source->thread_loop);
    return failure;
}

void streamlib_pipewire_video_source_close(struct StreamLibPipeWireVideoSource *video_source)
{
    if (video_source == NULL)
        return;
    destroy_video_source(video_source);
}

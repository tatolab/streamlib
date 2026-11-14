// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only
//
// The compiled half of the virtual camera's PipeWire door.
//
// SPA's video format pods, its buffer and meta parameters, and its buffer
// accessors are `static inline` C with no shared object behind them, so
// `dlopen` cannot reach them and they have to be compiled in. This translation
// unit is what compiles them, and it reaches libpipewire only through the
// entry-point table in `pipewire_entry_points.h`.

#ifndef STREAMLIB_PIPEWIRE_VIDEO_SOURCE_SHIM_H
#define STREAMLIB_PIPEWIRE_VIDEO_SOURCE_SHIM_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "pipewire_entry_points.h"

#ifdef __cplusplus
extern "C" {
#endif

/// How many properties [`streamlib_pipewire_video_source_properties`] declares.
/// Shared so the array and the function that fills it cannot disagree about
/// its size.
#define STREAMLIB_PIPEWIRE_VIDEO_SOURCE_PROPERTY_COUNT 6

/// The longest `node.name` derived from a camera name, with its NUL.
#define STREAMLIB_PIPEWIRE_VIDEO_NODE_NAME_CAPACITY 128

/// How many buffers one source may offer.
#define STREAMLIB_PIPEWIRE_VIDEO_MAX_BUFFERS 8

/// The picture one camera offers.
struct StreamLibPipeWireVideoOfferedFormat {
    uint32_t width;
    uint32_t height;
    uint32_t framerate_numerator;
    uint32_t framerate_denominator;
};

/// What a camera's `EnumFormat` pod actually says, read back out of the built
/// pod rather than restated, so a test holds the offer with no session.
struct StreamLibPipeWireVideoOfferReport {
    uint32_t width;
    uint32_t height;
    /// The `spa_video_format` offered.
    uint32_t video_format;
    /// Whether the pod carries a modifier property; a shared-memory offer must
    /// not, or a consumer reads it as a DMA-BUF offer.
    bool carries_a_modifier;
    /// False when the pod would not build or parse.
    bool was_built;
};

/// Build the format a camera offers and read it back.
struct StreamLibPipeWireVideoOfferReport streamlib_pipewire_video_source_describe_offer(
    const struct StreamLibPipeWireVideoOfferedFormat *offered_format);

/// The `spa_video_format` a camera offers: packed 4:2:2, `YUY2`.
uint32_t streamlib_pipewire_video_source_offered_video_format(void);

/// One registered `Video/Source` node, owned by the shim.
struct StreamLibPipeWireVideoSource;

/// The `node.name` a camera called `camera_name` registers under, returning how
/// many bytes were written, or 0 when it would not fit.
///
/// Exposed rather than kept private so a test can hold it without a session:
/// `node.name` is an identifier a session manager and every `pw-dump` reader
/// index by, and a name carrying a space or a slash is one PipeWire silently
/// treats as a different node than the one the log claims.
size_t streamlib_pipewire_video_source_node_name(const char *camera_name, char *node_name,
                                                 size_t node_name_capacity);

/// Compose the properties a camera-role source announces itself with, returning
/// how many of `items` were filled.
///
/// Exposed rather than kept private because this is where the portal decision
/// is actually made: WirePlumber's portal access rule grants camera clients
/// exactly the nodes carrying `Video/Source` and `media.role = Camera`, so a
/// missing pair is a node no portal-based picker will ever list. `node_name` is
/// scratch the returned items borrow, so it must outlive them.
uint32_t streamlib_pipewire_video_source_properties(struct StreamLibPipeWireStreamProperty *items,
                                                    uint32_t item_capacity,
                                                    const char *camera_name, char *node_name,
                                                    size_t node_name_capacity);

/// Register a `Video/Source` node named `camera_name`.
///
/// The node exists from here until close, whether or not anything ever watches
/// it — a camera in the picker with nobody looking. It offers no format until
/// [`streamlib_pipewire_video_source_set_extent`] states one. Returns NULL with
/// `failure_text` filled.
struct StreamLibPipeWireVideoSource *streamlib_pipewire_video_source_open(
    const void *const *entry_points, const char *camera_name, char *failure_text,
    size_t failure_text_capacity);

/// Offer one extent as shared-memory YUY2 buffers, `buffer_count` by default and
/// as many as `STREAMLIB_PIPEWIRE_VIDEO_MAX_BUFFERS` where the consumer asks, activate the
/// stream, and replace any extent offered earlier.
int streamlib_pipewire_video_source_set_extent(
    struct StreamLibPipeWireVideoSource *video_source, uint32_t width, uint32_t height,
    uint32_t framerate_numerator, uint32_t framerate_denominator, uint32_t buffer_count,
    char *failure_text, size_t failure_text_capacity);

/// Whether a consumer has settled a format, so frames have somewhere to go.
bool streamlib_pipewire_video_source_consumer_negotiated(
    struct StreamLibPipeWireVideoSource *video_source);

/// The buffer slot the next frame is written into, or -1 when nothing is
/// streaming or every buffer is still with the consumer.
///
/// A slot an earlier frame took and never published is handed out again rather
/// than a fresh one being taken: there is no way to return an output buffer
/// unpublished — `pw_stream_queue_buffer` submits it — so a caller that fails
/// mid-frame simply tries the same slot next time.
int32_t streamlib_pipewire_video_source_dequeue_slot(
    struct StreamLibPipeWireVideoSource *video_source);

/// Where slot `slot` maps, its row stride and its size; NULL for a slot that
/// holds no memory.
uint8_t *streamlib_pipewire_video_source_slot_shared_memory(
    struct StreamLibPipeWireVideoSource *video_source, int32_t slot, uint32_t *stride_bytes_out,
    uint32_t *byte_size_out);

/// Publish slot `slot`, stamped `timestamp_ns` in its header metadata, and
/// drive a graph cycle. The slot is released only once the buffer is on its
/// way, so a refused publish leaves it held for the next frame.
///
/// The stamp travels in `SPA_META_Header.pts` rather than `pw_buffer.time`:
/// `pw_buffer` is allocated by the host's libpipewire and only grew a `time`
/// field in 1.0.5, so writing one on the 0.3.50 floor these arms bind against
/// would write past the struct. The header meta is also what every consumer
/// actually reads.
int streamlib_pipewire_video_source_queue_slot(struct StreamLibPipeWireVideoSource *video_source,
                                               int32_t slot, int64_t timestamp_ns,
                                               uint64_t sequence);

/// The reason the stream failed after it was opened, or NULL while it is
/// healthy. Valid until the next call on this source.
const char *streamlib_pipewire_video_source_failure(
    struct StreamLibPipeWireVideoSource *video_source);

/// Disconnect, tear down and free the source; the node is gone with it. Safe
/// on NULL.
void streamlib_pipewire_video_source_close(struct StreamLibPipeWireVideoSource *video_source);

#ifdef __cplusplus
}
#endif

#endif /* STREAMLIB_PIPEWIRE_VIDEO_SOURCE_SHIM_H */

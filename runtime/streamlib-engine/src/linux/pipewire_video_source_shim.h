// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1
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

/// How a consumer settled on carrying this source's pixels.
enum StreamLibPipeWireVideoBufferKind {
    /// No consumer has negotiated a format yet.
    STREAMLIB_PIPEWIRE_VIDEO_BUFFER_KIND_NONE = 0,
    /// The consumer imports the engine's textures by file descriptor.
    STREAMLIB_PIPEWIRE_VIDEO_BUFFER_KIND_DMA_BUF = 1,
    /// The consumer took the shared-memory sibling; the shim allocates the
    /// memory and the caller copies read-back pixels into it.
    STREAMLIB_PIPEWIRE_VIDEO_BUFFER_KIND_SHARED_MEMORY = 2,
};

/// How many properties [`streamlib_pipewire_video_source_properties`] declares.
/// Shared so the array and the function that fills it cannot disagree about
/// its size.
#define STREAMLIB_PIPEWIRE_VIDEO_SOURCE_PROPERTY_COUNT 6

/// The longest `node.name` derived from a camera name, with its NUL.
#define STREAMLIB_PIPEWIRE_VIDEO_NODE_NAME_CAPACITY 128

/// How many buffers one source may offer. PipeWire is told this exact count
/// rather than a range, because the engine allocates and exports every one of
/// them before the stream can negotiate — a count the daemon raised would name
/// a texture that does not exist.
#define STREAMLIB_PIPEWIRE_VIDEO_MAX_BUFFERS 8

/// One engine-owned DMA-BUF the source hands to one PipeWire buffer.
///
/// The descriptor stays owned by the caller: the shim writes it into
/// `spa_data` and never closes it, so its lifetime is the caller's texture's.
struct StreamLibPipeWireVideoDmaBufPlane {
    int32_t file_descriptor;
    uint32_t stride_bytes;
    uint32_t offset_bytes;
    uint32_t byte_size;
};

/// The picture one camera offers, and the tiled DMA-BUF modifier its buffers
/// carry.
struct StreamLibPipeWireVideoOfferedFormat {
    uint32_t width;
    uint32_t height;
    uint32_t framerate_numerator;
    uint32_t framerate_denominator;
    uint64_t drm_modifier;
};

/// What a camera's pair of `EnumFormat` pods actually says, read back out of
/// the built pods rather than restated.
///
/// Exposed so the offer can be held by a test on a machine with no session:
/// getting the modifier property's flags wrong is the difference between a
/// consumer importing the engine's tiled textures and one importing them as
/// linear memory they were never in — a picture that is silently wrong rather
/// than a negotiation that fails.
struct StreamLibPipeWireVideoOfferReport {
    /// The extent both formats carry, as parsed back.
    uint32_t width;
    uint32_t height;
    /// How many modifiers a consumer may pick from, not counting the
    /// choice's leading default.
    uint32_t dma_buf_modifier_count;
    int64_t dma_buf_modifier;
    bool dma_buf_modifier_is_mandatory;
    bool dma_buf_modifier_may_not_be_fixated;
    /// Whether the sibling carries a modifier property at all; it must not.
    bool shared_memory_format_carries_a_modifier;
    /// Zero when either pod would not build or parse.
    bool both_formats_were_built;
};

/// Build the pair of formats a camera offers and read them back.
struct StreamLibPipeWireVideoOfferReport streamlib_pipewire_video_source_describe_offer(
    const struct StreamLibPipeWireVideoOfferedFormat *offered_format, bool fixated);

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
/// [`streamlib_pipewire_video_source_set_extent`] states one, which is what
/// makes "the engine's buffers are exported before a consumer can negotiate"
/// true rather than merely likely. Returns NULL with `failure_text` filled.
struct StreamLibPipeWireVideoSource *streamlib_pipewire_video_source_open(
    const void *const *entry_points, const char *camera_name, char *failure_text,
    size_t failure_text_capacity);

/// Offer one extent: `plane_count` engine-exported DMA-BUFs carrying
/// `drm_modifier`, beside a shared-memory sibling the consumer may take
/// instead. Activates the stream, and replaces any extent offered earlier.
///
/// The shim duplicates each descriptor, so the caller's textures may close
/// theirs whenever they like.
///
/// This frees the previous extent's shared-memory sibling, so a caller holding
/// an RHI import of it must drop that import *before* calling — the shim's
/// mappings live exactly as long as the extent that allocated them, which is
/// what makes such an import safe to hold across frames at all.
int streamlib_pipewire_video_source_set_extent(
    struct StreamLibPipeWireVideoSource *video_source, uint32_t width, uint32_t height,
    uint32_t framerate_numerator, uint32_t framerate_denominator, uint64_t drm_modifier,
    const struct StreamLibPipeWireVideoDmaBufPlane *planes, uint32_t plane_count,
    char *failure_text, size_t failure_text_capacity);

/// Which kind of buffer a consumer settled on, one of
/// [`enum StreamLibPipeWireVideoBufferKind`].
uint32_t streamlib_pipewire_video_source_negotiated_buffer_kind(
    struct StreamLibPipeWireVideoSource *video_source);

/// The buffer slot the next frame is written into, or -1 when nothing is
/// streaming or every buffer is still with the consumer.
///
/// A slot names the caller's own plane at the same index, so a dequeued slot
/// tells the caller which texture to write without a second table.
///
/// A slot an earlier frame took and never published is handed out again rather
/// than a fresh one being taken: there is no way to return an output buffer
/// unpublished — `pw_stream_queue_buffer` submits it — so a caller that fails
/// mid-frame simply tries the same slot next time.
int32_t streamlib_pipewire_video_source_dequeue_slot(
    struct StreamLibPipeWireVideoSource *video_source);

/// Where slot `slot` maps on the shared-memory sibling, or NULL on the DMA-BUF
/// path where the consumer imports the caller's texture instead.
///
/// Stable for the offered extent's whole life: a consumer disconnecting and
/// reconnecting retires PipeWire's buffers but not this mapping.
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

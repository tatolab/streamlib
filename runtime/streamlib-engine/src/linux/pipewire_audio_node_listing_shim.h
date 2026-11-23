// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only
//
// The PipeWire audio arm's listing: every audio node the session's registry holds, and the
// default sink and source its `default` metadata names, read in one round trip.
//
// The registry and metadata calls are `static inline` interface calls, so they compile in here;
// the few exported entry points they need come through `pipewire_entry_points.h`.

#ifndef STREAMLIB_PIPEWIRE_AUDIO_NODE_LISTING_SHIM_H
#define STREAMLIB_PIPEWIRE_AUDIO_NODE_LISTING_SHIM_H

#include <stdbool.h>
#include <stddef.h>

#include "pipewire_entry_points.h"

#ifdef __cplusplus
extern "C" {
#endif

/// Longest node name the default sink or source is read into. A longer one is read as no
/// default rather than truncated into another node's name.
#define STREAMLIB_PIPEWIRE_MAX_DEFAULT_NODE_NAME_BYTES 256

/// What the listing calls, on PipeWire's thread loop, for each node whose `media.class` starts
/// with `Audio/`. `node_description` is NULL where the node carries none. Each string is valid
/// only for the length of the call.
typedef void (*StreamLibPipeWireAudioNodeListed)(void *context, const char *node_name,
                                                 const char *node_description,
                                                 const char *media_class);

/// List the session's audio nodes to `on_node_listed`, and fill `default_sink_name` and
/// `default_source_name` with the nodes the `default` metadata names, each empty where it names
/// none. Returns 0 once the session has answered, and non-zero with `failure_text` filled when
/// it could not be asked or did not answer.
int streamlib_pipewire_list_audio_nodes(const void *const *entry_points,
                                        StreamLibPipeWireAudioNodeListed on_node_listed,
                                        void *context, char *default_sink_name,
                                        char *default_source_name, char *failure_text,
                                        size_t failure_text_capacity);

/// Read the node name out of a `default.audio.sink`-style metadata value, `{"name":"<node>"}`,
/// into `node_name`. Returns false, leaving it empty, for a value that names none or one longer
/// than `node_name_capacity` allows.
///
/// Exposed so a test can hold the parsing without a session: getting it wrong resolves the
/// default output's monitor to nothing, or to a name no node has.
bool streamlib_pipewire_node_name_of_default_metadata_value(const char *value, char *node_name,
                                                            size_t node_name_capacity);

#ifdef __cplusplus
}
#endif

#endif /* STREAMLIB_PIPEWIRE_AUDIO_NODE_LISTING_SHIM_H */

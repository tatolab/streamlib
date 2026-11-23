// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

#include "pipewire_audio_node_listing_shim.h"

#include <stdio.h>
#include <string.h>

#include <pipewire/core.h>
#include <pipewire/extensions/metadata.h>
#include <pipewire/keys.h>
#include <pipewire/node.h>
#include <pipewire/proxy.h>
#include <pipewire/thread-loop.h>
#include <pipewire/type.h>
#include <spa/utils/dict.h>
#include <spa/utils/hook.h>
#include <spa/utils/json.h>

// Nothing in this file may call a `pw_*` or `spa_*` symbol directly. Everything libpipewire
// exports is reached through `entry_points`; the registry, metadata and JSON calls are
// `static inline` and compile in. `nm -u` on the object file naming any symbol beyond libc is
// the failure the arm exists to prevent, and `test_wheel_portability.py` is what catches it.

/// How long one wait for the session's answer lasts before the listing gives up on it.
#define STREAMLIB_PIPEWIRE_LISTING_WAIT_SECONDS 2

/// The metadata object the session keeps the default nodes in.
#define STREAMLIB_PIPEWIRE_DEFAULT_METADATA_NAME "default"

#define STREAMLIB_PIPEWIRE_DEFAULT_AUDIO_SINK_KEY "default.audio.sink"
#define STREAMLIB_PIPEWIRE_DEFAULT_AUDIO_SOURCE_KEY "default.audio.source"

/// What one listing holds while the session answers it. Touched by the loop thread inside the
/// callbacks and by the caller only while it holds the loop's lock.
struct StreamLibPipeWireAudioNodeListing {
    struct StreamLibPipeWireEntryPoints entry_points;
    struct pw_thread_loop *thread_loop;
    struct pw_core *core;
    struct pw_registry *registry;
    struct pw_metadata *default_metadata;
    struct spa_hook core_listener;
    struct spa_hook registry_listener;
    struct spa_hook default_metadata_listener;
    StreamLibPipeWireAudioNodeListed on_node_listed;
    void *context;
    char *default_sink_name;
    char *default_source_name;
    /// The sequence number of the sync whose answer ends the listing.
    int pending_sync;
    /// Whether the default metadata was bound since the last sync was sent: its properties
    /// arrive only after another round trip.
    bool default_metadata_bound_since_the_last_sync;
    bool answered;
    const char *failure_reason;
};

bool streamlib_pipewire_node_name_of_default_metadata_value(const char *value, char *node_name,
                                                            size_t node_name_capacity)
{
    if (node_name == NULL || node_name_capacity == 0)
        return false;
    node_name[0] = '\0';
    if (value == NULL)
        return false;

    struct spa_json document;
    struct spa_json object;
    spa_json_init(&document, value, strlen(value));
    if (spa_json_enter_object(&document, &object) <= 0)
        return false;
    char key[64];
    while (spa_json_get_string(&object, key, sizeof(key)) > 0) {
        if (strcmp(key, "name") == 0) {
            if (spa_json_get_string(&object, node_name, (int)node_name_capacity) <= 0) {
                node_name[0] = '\0';
                return false;
            }
            return node_name[0] != '\0';
        }
        const char *skipped_value;
        if (spa_json_next(&object, &skipped_value) <= 0)
            return false;
    }
    return false;
}

static int on_default_metadata_property(void *data, uint32_t subject, const char *key,
                                        const char *type, const char *value)
{
    (void)type;
    struct StreamLibPipeWireAudioNodeListing *listing = data;
    if (subject != PW_ID_CORE || key == NULL)
        return 0;
    if (strcmp(key, STREAMLIB_PIPEWIRE_DEFAULT_AUDIO_SINK_KEY) == 0)
        streamlib_pipewire_node_name_of_default_metadata_value(
            value, listing->default_sink_name, STREAMLIB_PIPEWIRE_MAX_DEFAULT_NODE_NAME_BYTES);
    else if (strcmp(key, STREAMLIB_PIPEWIRE_DEFAULT_AUDIO_SOURCE_KEY) == 0)
        streamlib_pipewire_node_name_of_default_metadata_value(
            value, listing->default_source_name, STREAMLIB_PIPEWIRE_MAX_DEFAULT_NODE_NAME_BYTES);
    return 0;
}

static const struct pw_metadata_events default_metadata_events = {
    PW_VERSION_METADATA_EVENTS,
    .property = on_default_metadata_property,
};

static void bind_the_default_metadata(struct StreamLibPipeWireAudioNodeListing *listing,
                                      uint32_t id, const char *type)
{
    listing->default_metadata =
        pw_registry_bind(listing->registry, id, type, PW_VERSION_METADATA, 0);
    if (listing->default_metadata == NULL)
        return;
    pw_metadata_add_listener(listing->default_metadata, &listing->default_metadata_listener,
                             &default_metadata_events, listing);
    listing->default_metadata_bound_since_the_last_sync = true;
}

static void on_registry_global(void *data, uint32_t id, uint32_t permissions, const char *type,
                               uint32_t version, const struct spa_dict *props)
{
    (void)permissions;
    (void)version;
    struct StreamLibPipeWireAudioNodeListing *listing = data;
    if (type == NULL || props == NULL)
        return;

    if (strcmp(type, PW_TYPE_INTERFACE_Node) == 0) {
        const char *media_class = spa_dict_lookup(props, PW_KEY_MEDIA_CLASS);
        const char *node_name = spa_dict_lookup(props, PW_KEY_NODE_NAME);
        if (media_class == NULL || node_name == NULL || strncmp(media_class, "Audio/", 6) != 0)
            return;
        const char *node_description = spa_dict_lookup(props, PW_KEY_NODE_DESCRIPTION);
        if (node_description == NULL)
            node_description = spa_dict_lookup(props, PW_KEY_NODE_NICK);
        listing->on_node_listed(listing->context, node_name, node_description, media_class);
        return;
    }

    if (strcmp(type, PW_TYPE_INTERFACE_Metadata) == 0 && listing->default_metadata == NULL) {
        const char *metadata_name = spa_dict_lookup(props, PW_KEY_METADATA_NAME);
        if (metadata_name != NULL &&
            strcmp(metadata_name, STREAMLIB_PIPEWIRE_DEFAULT_METADATA_NAME) == 0)
            bind_the_default_metadata(listing, id, type);
    }
}

static const struct pw_registry_events registry_events = {
    PW_VERSION_REGISTRY_EVENTS,
    .global = on_registry_global,
};

static void on_core_done(void *data, uint32_t id, int seq)
{
    struct StreamLibPipeWireAudioNodeListing *listing = data;
    if (id != PW_ID_CORE || seq != listing->pending_sync)
        return;
    // The registry has announced every global by now; a metadata object bound while it did
    // sends its properties only after one more round trip.
    if (listing->default_metadata_bound_since_the_last_sync) {
        listing->default_metadata_bound_since_the_last_sync = false;
        listing->pending_sync = pw_core_sync(listing->core, PW_ID_CORE, listing->pending_sync);
        return;
    }
    listing->answered = true;
    listing->entry_points.pw_thread_loop_signal(listing->thread_loop, false);
}

static void on_core_error(void *data, uint32_t id, int seq, int res, const char *message)
{
    (void)seq;
    (void)res;
    (void)message;
    struct StreamLibPipeWireAudioNodeListing *listing = data;
    // An error on another object, the metadata among them, leaves the listing to finish
    // without what that object would have said.
    if (id != PW_ID_CORE)
        return;
    listing->failure_reason = "the PipeWire session refused the connection the listing asked on";
    listing->entry_points.pw_thread_loop_signal(listing->thread_loop, false);
}

static const struct pw_core_events core_events = {
    PW_VERSION_CORE_EVENTS,
    .done = on_core_done,
    .error = on_core_error,
};

/// Ask the session through `core` and wait, with the loop's lock held, until it has answered,
/// refused or gone quiet.
static void ask_the_session(struct StreamLibPipeWireAudioNodeListing *listing)
{
    pw_core_add_listener(listing->core, &listing->core_listener, &core_events, listing);
    listing->registry = pw_core_get_registry(listing->core, PW_VERSION_REGISTRY, 0);
    if (listing->registry == NULL) {
        listing->failure_reason = "the PipeWire session would not hand over its registry";
    } else {
        pw_registry_add_listener(listing->registry, &listing->registry_listener,
                                 &registry_events, listing);
        listing->pending_sync = pw_core_sync(listing->core, PW_ID_CORE, 0);
        if (listing->pending_sync < 0)
            listing->failure_reason = "the PipeWire session would not take the listing's sync";
        while (!listing->answered && listing->failure_reason == NULL) {
            if (listing->entry_points.pw_thread_loop_timed_wait(
                    listing->thread_loop, STREAMLIB_PIPEWIRE_LISTING_WAIT_SECONDS) != 0) {
                listing->failure_reason = "the PipeWire session did not answer the listing in time";
            }
        }
    }

    if (listing->default_metadata != NULL) {
        spa_hook_remove(&listing->default_metadata_listener);
        listing->entry_points.pw_proxy_destroy((struct pw_proxy *)listing->default_metadata);
        listing->default_metadata = NULL;
    }
    if (listing->registry != NULL) {
        spa_hook_remove(&listing->registry_listener);
        listing->entry_points.pw_proxy_destroy((struct pw_proxy *)listing->registry);
        listing->registry = NULL;
    }
    spa_hook_remove(&listing->core_listener);
}

int streamlib_pipewire_list_audio_nodes(const void *const *entry_points,
                                        StreamLibPipeWireAudioNodeListed on_node_listed,
                                        void *context, char *default_sink_name,
                                        char *default_source_name, char *failure_text,
                                        size_t failure_text_capacity)
{
    struct StreamLibPipeWireAudioNodeListing listing;
    memset(&listing, 0, sizeof(listing));
    streamlib_pipewire_copy_entry_points(&listing.entry_points, entry_points);
    listing.on_node_listed = on_node_listed;
    listing.context = context;
    listing.default_sink_name = default_sink_name;
    listing.default_source_name = default_source_name;
    default_sink_name[0] = '\0';
    default_source_name[0] = '\0';

    const struct StreamLibPipeWireEntryPoints *resolved = &listing.entry_points;
    listing.thread_loop = resolved->pw_thread_loop_new("streamlib-pw-list", NULL);
    if (listing.thread_loop == NULL) {
        streamlib_pipewire_copy_failure_text(failure_text, failure_text_capacity,
                                             "libpipewire would not create a thread loop");
        return 1;
    }

    resolved->pw_thread_loop_lock(listing.thread_loop);
    if (resolved->pw_thread_loop_start(listing.thread_loop) < 0) {
        listing.failure_reason = "libpipewire's thread loop would not start";
    } else {
        struct pw_context *pipewire_context =
            resolved->pw_context_new(resolved->pw_thread_loop_get_loop(listing.thread_loop), NULL,
                                     0);
        if (pipewire_context == NULL) {
            listing.failure_reason = "libpipewire would not create a context";
        } else {
            listing.core = resolved->pw_context_connect(pipewire_context, NULL, 0);
            if (listing.core == NULL) {
                listing.failure_reason = "no PipeWire daemon answered";
            } else {
                ask_the_session(&listing);
                resolved->pw_core_disconnect(listing.core);
                listing.core = NULL;
            }
            resolved->pw_context_destroy(pipewire_context);
        }
    }
    resolved->pw_thread_loop_unlock(listing.thread_loop);
    resolved->pw_thread_loop_stop(listing.thread_loop);
    resolved->pw_thread_loop_destroy(listing.thread_loop);

    if (listing.failure_reason != NULL) {
        streamlib_pipewire_copy_failure_text(failure_text, failure_text_capacity,
                                             listing.failure_reason);
        return 1;
    }
    return 0;
}

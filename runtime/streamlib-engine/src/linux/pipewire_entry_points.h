// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1
//
// The one list of libpipewire entry points every PipeWire shim calls through.
//
// SPA's pod builders and parsers are `static inline` C with no shared object
// behind them, so they cannot be reached by `dlopen` at all — they have to be
// compiled in. The shims are what compile them, and they reach libpipewire only
// through function pointers `linux/pipewire_runtime_library.rs` filled with
// `dlsym`. Nothing here references an external symbol of its own beyond libc,
// which is what keeps `libpipewire-0.3.so.0` out of the wheel's `DT_NEEDED` set.

#ifndef STREAMLIB_PIPEWIRE_ENTRY_POINTS_H
#define STREAMLIB_PIPEWIRE_ENTRY_POINTS_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include <pipewire/stream.h>
#include <pipewire/thread-loop.h>

#ifdef __cplusplus
extern "C" {
#endif

// The libpipewire entry points the shims call, in the order Rust must fill
// them. One X-macro generates the typed struct, the name array Rust resolves
// against, and the count — so the three cannot drift apart, which is the whole
// reason the list is written this way rather than three times.
//
// One list for every arm rather than one per shim: `dlsym` resolution and
// `pw_init` are process-global, and an audio backend and a virtual camera in
// the same graph must reach the same libpipewire.
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
                (struct pw_stream * stream, struct pw_time *time, size_t size))                    \
    ENTRY_POINT(pw_stream_update_params, int,                                                      \
                (struct pw_stream * stream, const struct spa_pod **params, uint32_t n_params))     \
    ENTRY_POINT(pw_stream_set_active, int, (struct pw_stream * stream, bool active))               \
    ENTRY_POINT(pw_stream_trigger_process, int, (struct pw_stream * stream))                       \
    ENTRY_POINT(pw_stream_get_state, enum pw_stream_state,                                         \
                (struct pw_stream * stream, const char **error))

/// The typed view of the pointer array Rust fills.
struct StreamLibPipeWireEntryPoints {
#define STREAMLIB_PIPEWIRE_DECLARE_ENTRY_POINT(name, return_type, parameters)                      \
    return_type (*name) parameters;
    STREAMLIB_PIPEWIRE_ENTRY_POINTS(STREAMLIB_PIPEWIRE_DECLARE_ENTRY_POINT)
#undef STREAMLIB_PIPEWIRE_DECLARE_ENTRY_POINT
};

/// Take a private copy of the table Rust filled.
///
/// Rust writes the table as a flat array of `dlsym` results and never names a
/// field, so "one pointer per name, in order" is the whole contract between the
/// two halves.
static inline void streamlib_pipewire_copy_entry_points(struct StreamLibPipeWireEntryPoints *into,
                                                        const void *const *entry_points)
{
    memcpy(into, entry_points, sizeof(*into));
}

/// One key/value pair of the property dict a stream announces itself with —
/// `struct spa_dict_item` by another name, so a test can read the composition
/// without the SPA headers.
struct StreamLibPipeWireStreamProperty {
    const char *key;
    const char *value;
};

/// How many entry points [`streamlib_pipewire_entry_point_names`] returns.
size_t streamlib_pipewire_entry_point_count(void);

/// The entry-point names to `dlsym`, in the order every shim expects the filled
/// pointer array to carry them.
const char *const *streamlib_pipewire_entry_point_names(void);

/// Initialize libpipewire's process-global state. Called once per process,
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

/// Copy `text` into a caller's bounded failure buffer, tolerating a caller
/// that offered none.
static inline void streamlib_pipewire_copy_failure_text(char *failure_text,
                                                        size_t failure_text_capacity,
                                                        const char *text)
{
    if (failure_text == NULL || failure_text_capacity == 0)
        return;
    snprintf(failure_text, failure_text_capacity, "%s", text);
}

#ifdef __cplusplus
}
#endif

#endif /* STREAMLIB_PIPEWIRE_ENTRY_POINTS_H */

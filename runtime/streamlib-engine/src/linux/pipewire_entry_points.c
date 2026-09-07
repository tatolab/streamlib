// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#include "pipewire_entry_points.h"

#include <pipewire/context.h>
#include <pipewire/core.h>

// Nothing in this file may call a `pw_*` or `spa_*` symbol directly. Everything
// libpipewire exports is reached through `entry_points`. `nm -u` on the object
// file naming any symbol beyond libc is the failure this arm exists to prevent,
// and `test_wheel_portability.py` is what catches it.

static const char *const kEntryPointNames[] = {
#define STREAMLIB_PIPEWIRE_NAME_ENTRY_POINT(name, return_type, parameters) #name,
    STREAMLIB_PIPEWIRE_ENTRY_POINTS(STREAMLIB_PIPEWIRE_NAME_ENTRY_POINT)
#undef STREAMLIB_PIPEWIRE_NAME_ENTRY_POINT
};

// Rust writes the table as a flat array of `dlsym` results and never names a
// field, and `streamlib_pipewire_copy_entry_points` memcpy's `sizeof(struct)`
// bytes out of that array. This is what pins the two to the same length:
// padding, or a compiler that sized a member differently, would make that read
// run past the end of what Rust allocated.
_Static_assert(sizeof(struct StreamLibPipeWireEntryPoints) ==
                   sizeof(kEntryPointNames) / sizeof(kEntryPointNames[0]) * sizeof(void (*)(void)),
               "the entry-point struct must be exactly one function pointer per resolved name");

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
    streamlib_pipewire_copy_entry_points(&resolved, entry_points);
    resolved.pw_init(NULL, NULL);
}

const char *streamlib_pipewire_loaded_library_version(const void *const *entry_points)
{
    struct StreamLibPipeWireEntryPoints resolved;
    streamlib_pipewire_copy_entry_points(&resolved, entry_points);
    return resolved.pw_get_library_version();
}

int streamlib_pipewire_daemon_answers(const void *const *entry_points, char *failure_text,
                                      size_t failure_text_capacity)
{
    struct StreamLibPipeWireEntryPoints resolved;
    streamlib_pipewire_copy_entry_points(&resolved, entry_points);

    struct pw_thread_loop *thread_loop = resolved.pw_thread_loop_new("streamlib-pw-probe", NULL);
    if (thread_loop == NULL) {
        streamlib_pipewire_copy_failure_text(failure_text, failure_text_capacity,
                          "libpipewire loaded but would not create a thread loop");
        return 1;
    }

    int verdict = 0;
    resolved.pw_thread_loop_lock(thread_loop);
    if (resolved.pw_thread_loop_start(thread_loop) < 0) {
        streamlib_pipewire_copy_failure_text(failure_text, failure_text_capacity,
                          "libpipewire loaded but its thread loop would not start");
        verdict = 1;
    } else {
        struct pw_context *context =
            resolved.pw_context_new(resolved.pw_thread_loop_get_loop(thread_loop), NULL, 0);
        if (context == NULL) {
            streamlib_pipewire_copy_failure_text(failure_text, failure_text_capacity,
                              "libpipewire loaded but would not create a context");
            verdict = 1;
        } else {
            struct pw_core *core = resolved.pw_context_connect(context, NULL, 0);
            if (core == NULL) {
                streamlib_pipewire_copy_failure_text(failure_text, failure_text_capacity,
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

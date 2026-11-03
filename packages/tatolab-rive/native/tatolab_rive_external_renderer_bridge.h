/* Copyright (c) 2025 Jonathan Fontanez
 * SPDX-License-Identifier: BUSL-1.1 */

/* The C surface this wheel's Rust reaches rive-runtime through. Every struct
 * here is laid out exactly as its `streamlib_consumer_rhi` namesake; the
 * Rust side asserts the offsets. */

#ifndef TATOLAB_RIVE_EXTERNAL_RENDERER_BRIDGE_H
#define TATOLAB_RIVE_EXTERNAL_RENDERER_BRIDGE_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* `ExternalRendererEnabledVulkanFeatures`: each a VkBool32. */
typedef struct TatolabRiveEnabledVulkanFeatures {
    uint32_t independent_blend;
    uint32_t fill_mode_non_solid;
    uint32_t fragment_stores_and_atomics;
    uint32_t shader_clip_distance;
    uint32_t sampler_anisotropy;
    uint32_t depth_bias_clamp;
} TatolabRiveEnabledVulkanFeatures;

/* `ExternalRendererRawVulkanHandles`. */
typedef struct TatolabRiveDeviceVulkanHandles {
    uint64_t vk_instance;
    uint64_t vk_physical_device;
    uint64_t vk_device;
    uint64_t vk_graphics_queue;
    uint64_t vk_get_instance_proc_addr;
    uint32_t graphics_queue_family_index;
    uint32_t vulkan_api_version;
    TatolabRiveEnabledVulkanFeatures enabled_features;
} TatolabRiveDeviceVulkanHandles;

/* `ExternalRendererFrameRawVulkanHandles`. */
typedef struct TatolabRiveFrameVulkanHandles {
    uint64_t vk_command_buffer;
    uint64_t vk_target_image;
    uint64_t vk_target_image_view;
    int32_t vk_target_format;
    int32_t vk_target_layout;
    uint32_t vk_target_last_pipeline_stages;
    uint32_t vk_target_last_access_mask;
    uint32_t vk_target_usage_flags;
    uint32_t target_width;
    uint32_t target_height;
    uint64_t current_frame_number;
    uint64_t safe_frame_number;
} TatolabRiveFrameVulkanHandles;

/* A call's outcome; a refusal's message is written to the caller's buffer. */
typedef int32_t TatolabRiveStatus;
#define TATOLAB_RIVE_STATUS_OK 0
#define TATOLAB_RIVE_STATUS_REFUSED (-1)

/* A view-model property's type, as `rive::DataType` numbers it; absent when
 * the path names no property. */
typedef int32_t TatolabRiveViewModelPropertyType;
#define TATOLAB_RIVE_VIEW_MODEL_PROPERTY_ABSENT (-1)
#define TATOLAB_RIVE_VIEW_MODEL_PROPERTY_STRING 1
#define TATOLAB_RIVE_VIEW_MODEL_PROPERTY_NUMBER 2
#define TATOLAB_RIVE_VIEW_MODEL_PROPERTY_BOOLEAN 3
#define TATOLAB_RIVE_VIEW_MODEL_PROPERTY_COLOR 4
#define TATOLAB_RIVE_VIEW_MODEL_PROPERTY_ENUM 6
#define TATOLAB_RIVE_VIEW_MODEL_PROPERTY_TRIGGER 7

/* A `.riv` file's artboard and state machine, playing on a Rive render
 * context built on the caller's device. */
typedef struct TatolabRiveScene TatolabRiveScene;

/* Build a render context on `device`, import `riv_bytes` and instance the
 * artboard and state machine named (the file's defaults for an empty
 * name), bound to the artboard's default view-model instance. NULL on
 * refusal, with the reason written to `error_message`. */
TatolabRiveScene *tatolab_rive_scene_create(const TatolabRiveDeviceVulkanHandles *device,
                                            const uint8_t *riv_bytes,
                                            size_t riv_byte_count,
                                            const char *artboard_name,
                                            const char *state_machine_name,
                                            char *error_message,
                                            size_t error_message_capacity);

/* Release every GPU resource the scene holds. The device's queue must be
 * idle of the scene's work. */
void tatolab_rive_scene_destroy(TatolabRiveScene *scene);

/* The artboard's name, NUL-terminated, valid while the scene lives. */
const char *tatolab_rive_scene_artboard_name(const TatolabRiveScene *scene);

/* The state machine's name, NUL-terminated, valid while the scene lives;
 * empty when the artboard has none and is drawn still. */
const char *tatolab_rive_scene_state_machine_name(const TatolabRiveScene *scene);

/* The type of the bound view-model instance's property at `property_path`. */
TatolabRiveViewModelPropertyType tatolab_rive_scene_view_model_property_type(
    const TatolabRiveScene *scene, const char *property_path);

/* Set a number, string, boolean, ARGB colour or enum property, or fire a
 * trigger, at `property_path`. Refused when the path names no property of
 * that type. */
TatolabRiveStatus tatolab_rive_scene_set_number(TatolabRiveScene *scene,
                                                const char *property_path,
                                                float value);
TatolabRiveStatus tatolab_rive_scene_set_string(TatolabRiveScene *scene,
                                                const char *property_path,
                                                const char *value_utf8,
                                                size_t value_byte_count);
TatolabRiveStatus tatolab_rive_scene_set_boolean(TatolabRiveScene *scene,
                                                 const char *property_path,
                                                 uint32_t value);
TatolabRiveStatus tatolab_rive_scene_set_color(TatolabRiveScene *scene,
                                               const char *property_path,
                                               uint32_t argb);
TatolabRiveStatus tatolab_rive_scene_set_enum(TatolabRiveScene *scene,
                                              const char *property_path,
                                              const char *value_utf8,
                                              size_t value_byte_count);
TatolabRiveStatus tatolab_rive_scene_fire_trigger(TatolabRiveScene *scene,
                                                  const char *property_path);

/* Advance the state machine by `elapsed_seconds` and record the artboard,
 * fit inside the target and centred over a transparent clear, into the
 * frame's command buffer. Writes the `VkImageLayout` the recorded commands
 * leave the target in. */
TatolabRiveStatus tatolab_rive_scene_advance_and_record(TatolabRiveScene *scene,
                                                        float elapsed_seconds,
                                                        const TatolabRiveFrameVulkanHandles *frame,
                                                        int32_t *target_layout_after_recording,
                                                        char *error_message,
                                                        size_t error_message_capacity);

#ifdef __cplusplus
}
#endif

#endif /* TATOLAB_RIVE_EXTERNAL_RENDERER_BRIDGE_H */

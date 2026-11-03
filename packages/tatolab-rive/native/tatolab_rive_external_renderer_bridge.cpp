/* Copyright (c) 2025 Jonathan Fontanez
 * SPDX-License-Identifier: BUSL-1.1 */

/* rive-runtime on a device the consumer RHI created: every Vulkan call here
 * is rive-runtime's own, made through the handles the RHI handed out. */

#include "tatolab_rive_external_renderer_bridge.h"

#include <cstdio>
#include <cstring>
#include <memory>
#include <string>

#include <vulkan/vulkan.h>

#include "rive/animation/state_machine_instance.hpp"
#include "rive/artboard.hpp"
#include "rive/file.hpp"
#include "rive/renderer.hpp"
#include "rive/renderer/render_context.hpp"
#include "rive/renderer/rive_renderer.hpp"
#include "rive/renderer/vulkan/render_context_vulkan_impl.hpp"
#include "rive/renderer/vulkan/render_target_vulkan.hpp"
#include "rive/scene.hpp"
#include "rive/static_scene.hpp"
#include "rive/viewmodel/runtime/viewmodel_instance_boolean_runtime.hpp"
#include "rive/viewmodel/runtime/viewmodel_instance_color_runtime.hpp"
#include "rive/viewmodel/runtime/viewmodel_instance_enum_runtime.hpp"
#include "rive/viewmodel/runtime/viewmodel_instance_number_runtime.hpp"
#include "rive/viewmodel/runtime/viewmodel_instance_runtime.hpp"
#include "rive/viewmodel/runtime/viewmodel_instance_string_runtime.hpp"
#include "rive/viewmodel/runtime/viewmodel_instance_trigger_runtime.hpp"

namespace
{

template <typename VulkanHandle> VulkanHandle vulkan_handle_of(uint64_t raw_handle)
{
    return reinterpret_cast<VulkanHandle>(static_cast<uintptr_t>(raw_handle));
}

void write_refusal(char *error_message, size_t error_message_capacity, const std::string &refusal)
{
    if (error_message == nullptr || error_message_capacity == 0)
    {
        return;
    }
    const size_t written = std::min(refusal.size(), error_message_capacity - 1);
    std::memcpy(error_message, refusal.data(), written);
    error_message[written] = '\0';
}

rive::gpu::VulkanFeatures rive_vulkan_features_of(const TatolabRiveDeviceVulkanHandles &device)
{
    rive::gpu::VulkanFeatures features;
    features.apiVersion = device.vulkan_api_version;
    features.independentBlend = device.enabled_features.independent_blend != 0;
    features.fillModeNonSolid = device.enabled_features.fill_mode_non_solid != 0;
    features.fragmentStoresAndAtomics = device.enabled_features.fragment_stores_and_atomics != 0;
    features.shaderClipDistance = device.enabled_features.shader_clip_distance != 0;
    features.samplerAnisotropy = device.enabled_features.sampler_anisotropy != 0;
    features.depthBiasClamp = device.enabled_features.depth_bias_clamp != 0;
    return features;
}

} // namespace

struct TatolabRiveScene
{
    std::unique_ptr<rive::gpu::RenderContext> render_context;
    rive::rcp<rive::gpu::RenderTargetVulkanImpl> render_target;
    rive::rcp<rive::File> file;
    std::unique_ptr<rive::ArtboardInstance> artboard;
    std::unique_ptr<rive::Scene> scene;
    rive::rcp<rive::ViewModelInstance> view_model_instance;
    rive::rcp<rive::ViewModelInstanceRuntime> view_model_instance_runtime;
    std::string artboard_name;
    std::string state_machine_name;

    rive::gpu::RenderContextVulkanImpl *vulkan_render_context_impl() const
    {
        return render_context->static_impl_cast<rive::gpu::RenderContextVulkanImpl>();
    }

    rive::ViewModelInstanceValueRuntime *view_model_property(const char *property_path) const
    {
        if (view_model_instance_runtime == nullptr || property_path == nullptr)
        {
            return nullptr;
        }
        return view_model_instance_runtime->property(property_path);
    }

    template <typename PropertyRuntime>
    PropertyRuntime *view_model_property_of_type(const char *property_path, rive::DataType type) const
    {
        rive::ViewModelInstanceValueRuntime *property = view_model_property(property_path);
        if (property == nullptr || property->dataType() != type)
        {
            return nullptr;
        }
        return static_cast<PropertyRuntime *>(property);
    }
};

extern "C" TatolabRiveScene *tatolab_rive_scene_create(const TatolabRiveDeviceVulkanHandles *device,
                                                       const uint8_t *riv_bytes,
                                                       size_t riv_byte_count,
                                                       const char *artboard_name,
                                                       const char *state_machine_name,
                                                       char *error_message,
                                                       size_t error_message_capacity)
{
    auto scene = std::make_unique<TatolabRiveScene>();
    scene->render_context = rive::gpu::RenderContextVulkanImpl::MakeContext(
        vulkan_handle_of<VkInstance>(device->vk_instance),
        vulkan_handle_of<VkPhysicalDevice>(device->vk_physical_device),
        vulkan_handle_of<VkDevice>(device->vk_device),
        rive_vulkan_features_of(*device),
        reinterpret_cast<PFN_vkGetInstanceProcAddr>(
            static_cast<uintptr_t>(device->vk_get_instance_proc_addr)));
    if (scene->render_context == nullptr)
    {
        write_refusal(error_message,
                      error_message_capacity,
                      "rive-runtime could not build a Vulkan render context on the device");
        return nullptr;
    }

    rive::ImportResult import_result = rive::ImportResult::success;
    scene->file = rive::File::import(rive::Span<const uint8_t>(riv_bytes, riv_byte_count),
                                     scene->render_context.get(),
                                     &import_result);
    if (scene->file == nullptr)
    {
        write_refusal(error_message,
                      error_message_capacity,
                      import_result == rive::ImportResult::unsupportedVersion
                          ? "the .riv file was written by an unsupported Rive version"
                          : "the .riv file is malformed");
        return nullptr;
    }

    const bool artboard_is_named = artboard_name != nullptr && artboard_name[0] != '\0';
    scene->artboard = artboard_is_named ? scene->file->artboardNamed(artboard_name)
                                        : scene->file->artboardDefault();
    if (scene->artboard == nullptr)
    {
        write_refusal(error_message,
                      error_message_capacity,
                      artboard_is_named
                          ? std::string("the .riv file has no artboard named \"") + artboard_name + "\""
                          : std::string("the .riv file has no artboard"));
        return nullptr;
    }
    scene->artboard_name = scene->artboard->name();

    const bool state_machine_is_named =
        state_machine_name != nullptr && state_machine_name[0] != '\0';
    std::unique_ptr<rive::StateMachineInstance> state_machine =
        state_machine_is_named ? scene->artboard->stateMachineNamed(state_machine_name)
                               : scene->artboard->defaultStateMachine();
    if (state_machine == nullptr && !state_machine_is_named &&
        scene->artboard->stateMachineCount() > 0)
    {
        state_machine = scene->artboard->stateMachineAt(0);
    }
    if (state_machine == nullptr && state_machine_is_named)
    {
        write_refusal(error_message,
                      error_message_capacity,
                      std::string("artboard \"") + scene->artboard_name +
                          "\" has no state machine named \"" + state_machine_name + "\"");
        return nullptr;
    }
    if (state_machine != nullptr)
    {
        scene->state_machine_name = state_machine->name();
        scene->scene = std::move(state_machine);
    }
    else
    {
        scene->scene = std::make_unique<rive::StaticScene>(scene->artboard.get());
    }

    scene->view_model_instance = scene->file->createDefaultViewModelInstance(scene->artboard.get());
    if (scene->view_model_instance != nullptr)
    {
        scene->artboard->bindViewModelInstance(scene->view_model_instance);
        scene->scene->bindViewModelInstance(scene->view_model_instance);
        scene->view_model_instance_runtime =
            rive::make_rcp<rive::ViewModelInstanceRuntime>(scene->view_model_instance);
    }
    return scene.release();
}

extern "C" void tatolab_rive_scene_destroy(TatolabRiveScene *scene)
{
    if (scene == nullptr)
    {
        return;
    }
    scene->view_model_instance_runtime = nullptr;
    scene->scene.reset();
    scene->artboard.reset();
    scene->view_model_instance = nullptr;
    scene->file = nullptr;
    scene->render_target = nullptr;
    scene->render_context.reset();
    delete scene;
}

extern "C" const char *tatolab_rive_scene_artboard_name(const TatolabRiveScene *scene)
{
    return scene->artboard_name.c_str();
}

extern "C" const char *tatolab_rive_scene_state_machine_name(const TatolabRiveScene *scene)
{
    return scene->state_machine_name.c_str();
}

extern "C" TatolabRiveViewModelPropertyType tatolab_rive_scene_view_model_property_type(
    const TatolabRiveScene *scene, const char *property_path)
{
    rive::ViewModelInstanceValueRuntime *property = scene->view_model_property(property_path);
    if (property == nullptr)
    {
        return TATOLAB_RIVE_VIEW_MODEL_PROPERTY_ABSENT;
    }
    return static_cast<TatolabRiveViewModelPropertyType>(property->dataType());
}

extern "C" TatolabRiveStatus tatolab_rive_scene_set_number(TatolabRiveScene *scene,
                                                           const char *property_path,
                                                           float value)
{
    auto *property = scene->view_model_property_of_type<rive::ViewModelInstanceNumberRuntime>(
        property_path, rive::DataType::number);
    if (property == nullptr)
    {
        return TATOLAB_RIVE_STATUS_REFUSED;
    }
    property->value(value);
    return TATOLAB_RIVE_STATUS_OK;
}

extern "C" TatolabRiveStatus tatolab_rive_scene_set_string(TatolabRiveScene *scene,
                                                           const char *property_path,
                                                           const char *value_utf8,
                                                           size_t value_byte_count)
{
    auto *property = scene->view_model_property_of_type<rive::ViewModelInstanceStringRuntime>(
        property_path, rive::DataType::string);
    if (property == nullptr)
    {
        return TATOLAB_RIVE_STATUS_REFUSED;
    }
    property->value(std::string(value_utf8, value_byte_count));
    return TATOLAB_RIVE_STATUS_OK;
}

extern "C" TatolabRiveStatus tatolab_rive_scene_set_boolean(TatolabRiveScene *scene,
                                                            const char *property_path,
                                                            uint32_t value)
{
    auto *property = scene->view_model_property_of_type<rive::ViewModelInstanceBooleanRuntime>(
        property_path, rive::DataType::boolean);
    if (property == nullptr)
    {
        return TATOLAB_RIVE_STATUS_REFUSED;
    }
    property->value(value != 0);
    return TATOLAB_RIVE_STATUS_OK;
}

extern "C" TatolabRiveStatus tatolab_rive_scene_set_color(TatolabRiveScene *scene,
                                                          const char *property_path,
                                                          uint32_t argb)
{
    auto *property = scene->view_model_property_of_type<rive::ViewModelInstanceColorRuntime>(
        property_path, rive::DataType::color);
    if (property == nullptr)
    {
        return TATOLAB_RIVE_STATUS_REFUSED;
    }
    property->value(static_cast<int>(argb));
    return TATOLAB_RIVE_STATUS_OK;
}

extern "C" TatolabRiveStatus tatolab_rive_scene_set_enum(TatolabRiveScene *scene,
                                                         const char *property_path,
                                                         const char *value_utf8,
                                                         size_t value_byte_count)
{
    auto *property = scene->view_model_property_of_type<rive::ViewModelInstanceEnumRuntime>(
        property_path, rive::DataType::enumType);
    if (property == nullptr)
    {
        return TATOLAB_RIVE_STATUS_REFUSED;
    }
    const std::string value(value_utf8, value_byte_count);
    for (const std::string &declared_value : property->values())
    {
        if (declared_value == value)
        {
            property->value(value);
            return TATOLAB_RIVE_STATUS_OK;
        }
    }
    return TATOLAB_RIVE_STATUS_REFUSED;
}

extern "C" TatolabRiveStatus tatolab_rive_scene_fire_trigger(TatolabRiveScene *scene,
                                                             const char *property_path)
{
    auto *property = scene->view_model_property_of_type<rive::ViewModelInstanceTriggerRuntime>(
        property_path, rive::DataType::trigger);
    if (property == nullptr)
    {
        return TATOLAB_RIVE_STATUS_REFUSED;
    }
    property->trigger();
    return TATOLAB_RIVE_STATUS_OK;
}

extern "C" TatolabRiveStatus tatolab_rive_scene_advance_and_record(
    TatolabRiveScene *scene,
    float elapsed_seconds,
    const TatolabRiveFrameVulkanHandles *frame,
    int32_t *target_layout_after_recording,
    char *error_message,
    size_t error_message_capacity)
{
    const auto target_format = static_cast<VkFormat>(frame->vk_target_format);
    const auto target_usage = static_cast<VkImageUsageFlags>(frame->vk_target_usage_flags);
    if (scene->render_target == nullptr ||
        scene->render_target->width() != frame->target_width ||
        scene->render_target->height() != frame->target_height ||
        scene->render_target->framebufferFormat() != target_format ||
        scene->render_target->targetUsageFlags() != target_usage)
    {
        scene->render_target = scene->vulkan_render_context_impl()->makeRenderTarget(
            frame->target_width, frame->target_height, target_format, target_usage);
        if (scene->render_target == nullptr)
        {
            write_refusal(error_message,
                          error_message_capacity,
                          "rive-runtime could not make a render target of the frame's format");
            return TATOLAB_RIVE_STATUS_REFUSED;
        }
    }
    rive::gpu::vkutil::ImageAccess target_last_access;
    target_last_access.pipelineStages =
        static_cast<VkPipelineStageFlags>(frame->vk_target_last_pipeline_stages);
    target_last_access.accessMask = static_cast<VkAccessFlags>(frame->vk_target_last_access_mask);
    target_last_access.layout = static_cast<VkImageLayout>(frame->vk_target_layout);
    scene->render_target->setTargetImageView(
        vulkan_handle_of<VkImageView>(frame->vk_target_image_view),
        vulkan_handle_of<VkImage>(frame->vk_target_image),
        target_last_access);

    scene->scene->advanceAndApply(elapsed_seconds);

    rive::gpu::RenderContext::FrameDescriptor frame_descriptor;
    frame_descriptor.renderTargetWidth = frame->target_width;
    frame_descriptor.renderTargetHeight = frame->target_height;
    frame_descriptor.loadAction = rive::gpu::LoadAction::clear;
    frame_descriptor.clearColor = 0x00000000;
    scene->render_context->beginFrame(frame_descriptor);

    rive::RiveRenderer renderer(scene->render_context.get());
    renderer.save();
    renderer.align(rive::Fit::contain,
                   rive::Alignment::center,
                   rive::AABB(0,
                              0,
                              static_cast<float>(frame->target_width),
                              static_cast<float>(frame->target_height)),
                   scene->artboard->bounds());
    scene->scene->draw(&renderer);
    renderer.restore();

    rive::gpu::RenderContext::FlushResources flush_resources;
    flush_resources.renderTarget = scene->render_target.get();
    flush_resources.externalCommandBuffer = vulkan_handle_of<VkCommandBuffer>(frame->vk_command_buffer);
    flush_resources.currentFrameNumber = frame->current_frame_number;
    flush_resources.safeFrameNumber = frame->safe_frame_number;
    scene->render_context->flush(flush_resources);

    *target_layout_after_recording =
        static_cast<int32_t>(scene->render_target->targetLastAccess().layout);
    return TATOLAB_RIVE_STATUS_OK;
}

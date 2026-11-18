// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Built-in `TensorToTexture`: one `[h, w, 4]` attribute of each Array frame as
//! one Image frame.

use serde::{Deserialize, Serialize};
use streamlib::sdk::context::{
    NativeNodeOutputPixelBufferFrame, NodeOutputPixelBufferPoolOfOneNativeNode,
    RuntimeContextFullAccess, RuntimeContextLimitedAccess,
};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::iceoryx2::{
    ArrayFrameAttributeResidency, ArrayFrameDescriptorBody, DataChannelPayloadDescriptor,
    DataChannelPayloadDescriptorKind, ImageFrameDescriptorBody, ImageFrameH273Colour,
    array_frame_attribute_table_of,
};
use streamlib::sdk::nodes::ReactiveNode;
use streamlib::sdk::rhi::{PixelFormat, TextureFormat};
use streamlib::sdk::schemars::JsonSchema;

use super::port_family_messages_read_by_a_conversion::refuse_unless_the_descriptor_is;
use super::texture_to_tensor::{
    TEXTURE_TO_TENSOR_IMAGE_ATTRIBUTE_NAME, tensor_element_and_channels_of_one_pixel,
};

const ARRAY_INPUT_PORT: &str = "array";
const IMAGE_OUTPUT_PORT: &str = "image";

/// Configuration for [`TensorToTexture`].
#[streamlib::sdk::node_config]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(crate = "streamlib::sdk::schemars")]
#[serde(deny_unknown_fields)]
pub struct TensorToTextureConfig {
    #[serde(default = "default_image_attribute_name")]
    #[description = "The attribute of each Array frame read as the image."]
    pub attribute: String,
    #[serde(default = "default_image_format_wire_name")]
    #[description = "The texture format each Image frame states, by its wire name."]
    pub format: String,
}

fn default_image_attribute_name() -> String {
    TEXTURE_TO_TENSOR_IMAGE_ATTRIBUTE_NAME.to_string()
}

fn default_image_format_wire_name() -> String {
    TextureFormat::Rgba8Unorm.wire_name().to_string()
}

impl Default for TensorToTextureConfig {
    fn default() -> Self {
        Self {
            attribute: default_image_attribute_name(),
            format: default_image_format_wire_name(),
        }
    }
}

#[streamlib::sdk::node(
    built_in_node,
    execution = reactive,
    config = crate::pin_type_conversion::tensor_to_texture::TensorToTextureConfig,
    input(
        "array",
        description = "The Array frames whose `attribute` is converted",
        delivery_profile = "newest",
        pin_type = Tensor
    ),
    output(
        "image",
        description = "One Image frame per Array frame",
        pin_type = Texture,
        formats = [
            "rgba8_unorm",
            "rgba8_unorm_srgb",
            "bgra8_unorm",
            "bgra8_unorm_srgb",
            "rgba16_float",
            "rgba32_float"
        ]
    ),
)]
#[meta(
    conversion = true,
    description = "Converts one attribute of each Array frame on `array`, `attribute` (`image` \
                   by default), into one Image frame on `image` stating `format` \
                   (`rgba8_unorm` by default; also `rgba8_unorm_srgb`, `bgra8_unorm`, \
                   `bgra8_unorm_srgb`, `rgba16_float` and `rgba32_float`). The attribute is an \
                   `[h, w, 4]` tensor of the format's element type, its channels in the format's \
                   order: `uint8` for the 8-bit formats, `float16` for `rgba16_float` and \
                   `float32` for `rgba32_float`. A GPU attribute's pool slot becomes the Image \
                   frame's slot and nothing is copied, so the slot holds what `format` states, \
                   as an attribute `TextureToTensor` wrote does. A CPU attribute is copied once \
                   into a pixel buffer from this node's own output pool. A frame without the \
                   attribute, or whose attribute is of another dtype or another shape, is \
                   refused and writes nothing. Each Image frame states no colour."
)]
pub struct TensorToTexture {
    image_format: Option<TextureFormat>,
    output_pixel_buffer_pool: Option<NodeOutputPixelBufferPoolOfOneNativeNode>,
}

impl ReactiveNode for TensorToTexture::Node {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.image_format = Some(image_format_of_the_config(&self.config)?);
        let node_run_id_of_this_node = ctx
            .node_run_id()
            .unwrap_or_else(|| "TensorToTexture".to_string());
        self.output_pixel_buffer_pool = Some(
            ctx.gpu_full_access()
                .create_node_output_pixel_buffer_pool_for_a_native_node(&node_run_id_of_this_node),
        );
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        let (Some(image_format), Some(output_pixel_buffer_pool)) =
            (self.image_format, self.output_pixel_buffer_pool.as_mut())
        else {
            return Err(Error::Runtime(
                "TensorToTexture: process ran before setup".to_string(),
            ));
        };
        while let Some(message) = self.inputs.read_raw(ARRAY_INPUT_PORT)? {
            match image_frame_of_the_array_frame(
                &self.config.attribute,
                image_format,
                message.payload_descriptor,
                message.payload.bytes(),
                |width, height, pixel_format| {
                    output_pixel_buffer_pool.acquire_the_next_output_pixel_buffer(
                        width,
                        height,
                        pixel_format,
                    )
                },
            ) {
                Ok(payload_descriptor) => self.outputs.write_bytes_described_by(
                    IMAGE_OUTPUT_PORT,
                    &[],
                    message.timestamp_ns,
                    message.group_boundary,
                    payload_descriptor,
                )?,
                Err(refusal) => {
                    tracing::warn!(%refusal, "TensorToTexture wrote nothing for a message")
                }
            }
        }
        Ok(())
    }
}

/// The texture format `config.format` names, refused unless one `[h, w, c]`
/// tensor holds it.
pub fn image_format_of_the_config(config: &TensorToTextureConfig) -> Result<TextureFormat> {
    TextureFormat::from_wire_name(&config.format)
        .filter(|format| tensor_element_and_channels_of_one_pixel(*format).is_some())
        .ok_or_else(|| {
            Error::Configuration(format!(
                "TensorToTexture: `format` is {:?}; it is one of rgba8_unorm, rgba8_unorm_srgb, \
                 bgra8_unorm, bgra8_unorm_srgb, rgba16_float and rgba32_float",
                config.format
            ))
        })
}

/// The Image frame holding `attribute_name` of the Array frame
/// `payload_descriptor` and `payload` carry, as `image_format`: a GPU
/// attribute's own slot, or a CPU attribute copied into the pixel buffer
/// `acquire_output_pixel_buffer` hands out.
pub fn image_frame_of_the_array_frame(
    attribute_name: &str,
    image_format: TextureFormat,
    payload_descriptor: DataChannelPayloadDescriptor,
    payload: &[u8],
    acquire_output_pixel_buffer: impl FnOnce(
        u32,
        u32,
        PixelFormat,
    ) -> Result<NativeNodeOutputPixelBufferFrame>,
) -> Result<DataChannelPayloadDescriptor> {
    refuse_unless_the_descriptor_is(
        "TensorToTexture",
        ARRAY_INPUT_PORT,
        payload_descriptor,
        DataChannelPayloadDescriptorKind::ArrayFrame,
    )?;
    let attribute_table = array_frame_attribute_table_of(
        ArrayFrameDescriptorBody::from_descriptor_body(&payload_descriptor.body),
        payload,
    )
    .map_err(|refusal| Error::PortError(format!("TensorToTexture: {refusal}")))?;
    let attribute = attribute_table
        .iter()
        .find(|row| row.name == attribute_name)
        .ok_or_else(|| {
            Error::PortError(format!(
                "TensorToTexture: an Array frame on `array` has no attribute \
                 {attribute_name:?}; it has {:?}",
                attribute_table
                    .iter()
                    .map(|row| row.name.as_str())
                    .collect::<Vec<_>>()
            ))
        })?;
    let (image_dtype, image_channels) = tensor_element_and_channels_of_one_pixel(image_format)
        .ok_or_else(|| {
            Error::Configuration(format!(
                "TensorToTexture: {} has no one-tensor layout",
                image_format.wire_name()
            ))
        })?;
    if attribute.dtype != image_dtype {
        return Err(Error::PortError(format!(
            "TensorToTexture: attribute {attribute_name:?} is {}; a {} image is {image_dtype}",
            attribute.dtype,
            image_format.wire_name()
        )));
    }
    let (width, height) = match attribute.shape[..] {
        [height, width, channels] if channels == image_channels => (
            u32::try_from(width).ok().filter(|width| *width > 0),
            u32::try_from(height).ok().filter(|height| *height > 0),
        ),
        _ => (None, None),
    };
    let (Some(width), Some(height)) = (width, height) else {
        return Err(Error::PortError(format!(
            "TensorToTexture: attribute {attribute_name:?} has shape {:?}; it is read as [h, w, \
             {image_channels}]",
            attribute.shape
        )));
    };
    let slot = match attribute.residency {
        ArrayFrameAttributeResidency::GpuPoolSlot(attribute_slot) => attribute_slot,
        ArrayFrameAttributeResidency::Cpu {
            payload_byte_offset,
            byte_len,
        } => {
            let pixel_format = image_format.host_view_pixel_format().ok_or_else(|| {
                Error::Configuration(format!(
                    "TensorToTexture: no pixel buffer holds {}",
                    image_format.wire_name()
                ))
            })?;
            let attribute_bytes = payload
                .get(payload_byte_offset as usize..(payload_byte_offset + byte_len) as usize)
                .ok_or_else(|| {
                    Error::PortError(format!(
                        "TensorToTexture: attribute {attribute_name:?} runs past its payload"
                    ))
                })?;
            let output_frame = acquire_output_pixel_buffer(width, height, pixel_format)?;
            output_frame
                .pixel_buffer
                .write_this_plane_from(0, attribute_bytes)?;
            output_frame.gpu_pool_slot_frame.ok_or_else(|| {
                Error::GpuError(format!(
                    "TensorToTexture: pixel buffer {} has no claim-table row, so no reader could \
                     claim it",
                    output_frame.published_frame_id
                ))
            })?
        }
    };
    Ok(DataChannelPayloadDescriptor::image_frame(
        ImageFrameDescriptorBody {
            slot,
            width,
            height,
            format: image_format as u32,
            colour: ImageFrameH273Colour::UNSPECIFIED,
        },
    ))
}

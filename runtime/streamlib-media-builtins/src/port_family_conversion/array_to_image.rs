// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Built-in `ArrayToImage`: one `[h, w, 4]` attribute of each Array frame as
//! one Image frame.

use serde::{Deserialize, Serialize};
use streamlib::sdk::context::{
    NativeNodeOutputPixelBufferFrame, ProcessorOutputPixelBufferPoolOfOneNativeNode,
    RuntimeContextFullAccess, RuntimeContextLimitedAccess,
};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::iceoryx2::{
    ArrayFrameAttributeResidency, ArrayFrameDescriptorBody, DataChannelPayloadDescriptor,
    DataChannelPayloadDescriptorKind, ImageFrameDescriptorBody, ImageFrameH273Colour,
    array_frame_attribute_table_of,
};
use streamlib::sdk::processors::ReactiveProcessor;
use streamlib::sdk::rhi::{PixelFormat, TextureFormat};
use streamlib::sdk::schemars::JsonSchema;

use super::image_to_array::{
    IMAGE_TO_ARRAY_IMAGE_ATTRIBUTE_NAME, tensor_element_and_channels_of_one_pixel,
};
use super::port_family_messages_read_by_a_conversion::refuse_unless_the_descriptor_is;

const ARRAY_INPUT_PORT: &str = "array";
const IMAGE_OUTPUT_PORT: &str = "image";

/// Configuration for [`ArrayToImage`].
#[streamlib::sdk::processor_config]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(crate = "streamlib::sdk::schemars")]
#[serde(deny_unknown_fields)]
pub struct ArrayToImageConfig {
    #[serde(default = "default_image_attribute_name")]
    #[description = "The attribute of each Array frame read as the image."]
    pub attribute: String,
    #[serde(default = "default_image_format_wire_name")]
    #[description = "The texture format each Image frame states, by its wire name."]
    pub format: String,
}

fn default_image_attribute_name() -> String {
    IMAGE_TO_ARRAY_IMAGE_ATTRIBUTE_NAME.to_string()
}

fn default_image_format_wire_name() -> String {
    TextureFormat::Rgba8Unorm.wire_name().to_string()
}

impl Default for ArrayToImageConfig {
    fn default() -> Self {
        Self {
            attribute: default_image_attribute_name(),
            format: default_image_format_wire_name(),
        }
    }
}

#[streamlib::sdk::processor(
    built_in_node,
    execution = reactive,
    config = crate::port_family_conversion::array_to_image::ArrayToImageConfig,
    input(
        "array",
        description = "The Array frames whose `attribute` is converted",
        delivery_profile = "newest",
        family = Array
    ),
    output(
        "image",
        description = "One Image frame per Array frame",
        family = Image
    ),
)]
#[meta(
    description = "Converts one attribute of each Array frame on `array`, `attribute` (`image` \
                   by default), into one Image frame on `image` stating `format` \
                   (`rgba8_unorm` by default; also `rgba8_unorm_srgb`, `bgra8_unorm`, \
                   `bgra8_unorm_srgb`, `rgba16_float` and `rgba32_float`). The attribute is an \
                   `[h, w, 4]` tensor of the format's element type, its channels in the format's \
                   order: `uint8` for the 8-bit formats, `float16` for `rgba16_float` and \
                   `float32` for `rgba32_float`. A GPU attribute's pool slot becomes the Image \
                   frame's slot and nothing is copied, so the slot holds what `format` states, \
                   as an attribute `ImageToArray` wrote does. A CPU attribute is copied once into \
                   a pixel buffer from this node's own output pool. A frame without the \
                   attribute, or whose attribute is of another dtype or another shape, is \
                   refused and writes nothing. Each Image frame states no colour."
)]
pub struct ArrayToImage {
    image_format: Option<TextureFormat>,
    output_pixel_buffer_pool: Option<ProcessorOutputPixelBufferPoolOfOneNativeNode>,
}

impl ReactiveProcessor for ArrayToImage::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.image_format = Some(image_format_of_the_config(&self.config)?);
        let processor_id_of_this_node = ctx
            .processor_id()
            .unwrap_or_else(|| "ArrayToImage".to_string());
        self.output_pixel_buffer_pool = Some(
            ctx.gpu_full_access()
                .create_processor_output_pixel_buffer_pool_for_a_native_node(
                    &processor_id_of_this_node,
                ),
        );
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        let (Some(image_format), Some(output_pixel_buffer_pool)) =
            (self.image_format, self.output_pixel_buffer_pool.as_mut())
        else {
            return Err(Error::Runtime(
                "ArrayToImage: process ran before setup".to_string(),
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
                    tracing::warn!(%refusal, "ArrayToImage wrote nothing for a message")
                }
            }
        }
        Ok(())
    }
}

/// The texture format `config.format` names, refused unless one `[h, w, c]`
/// tensor holds it.
pub fn image_format_of_the_config(config: &ArrayToImageConfig) -> Result<TextureFormat> {
    TextureFormat::from_wire_name(&config.format)
        .filter(|format| tensor_element_and_channels_of_one_pixel(*format).is_some())
        .ok_or_else(|| {
            Error::Configuration(format!(
                "ArrayToImage: `format` is {:?}; it is one of rgba8_unorm, rgba8_unorm_srgb, \
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
        "ArrayToImage",
        ARRAY_INPUT_PORT,
        payload_descriptor,
        DataChannelPayloadDescriptorKind::ArrayFrame,
    )?;
    let attribute_table = array_frame_attribute_table_of(
        ArrayFrameDescriptorBody::from_descriptor_body(&payload_descriptor.body),
        payload,
    )
    .map_err(|refusal| Error::PortError(format!("ArrayToImage: {refusal}")))?;
    let attribute = attribute_table
        .iter()
        .find(|row| row.name == attribute_name)
        .ok_or_else(|| {
            Error::PortError(format!(
                "ArrayToImage: an Array frame on `array` has no attribute {attribute_name:?}; it \
                 has {:?}",
                attribute_table
                    .iter()
                    .map(|row| row.name.as_str())
                    .collect::<Vec<_>>()
            ))
        })?;
    let (image_dtype, image_channels) = tensor_element_and_channels_of_one_pixel(image_format)
        .ok_or_else(|| {
            Error::Configuration(format!(
                "ArrayToImage: {} has no one-tensor layout",
                image_format.wire_name()
            ))
        })?;
    if attribute.dtype != image_dtype {
        return Err(Error::PortError(format!(
            "ArrayToImage: attribute {attribute_name:?} is {}; a {} image is {image_dtype}",
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
            "ArrayToImage: attribute {attribute_name:?} has shape {:?}; it is read as [h, w, \
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
                    "ArrayToImage: no pixel buffer holds {}",
                    image_format.wire_name()
                ))
            })?;
            let attribute_bytes = payload
                .get(payload_byte_offset as usize..(payload_byte_offset + byte_len) as usize)
                .ok_or_else(|| {
                    Error::PortError(format!(
                        "ArrayToImage: attribute {attribute_name:?} runs past its payload"
                    ))
                })?;
            let output_frame = acquire_output_pixel_buffer(width, height, pixel_format)?;
            output_frame
                .pixel_buffer
                .write_this_plane_from(0, attribute_bytes)?;
            output_frame.gpu_pool_slot_frame.ok_or_else(|| {
                Error::GpuError(format!(
                    "ArrayToImage: pixel buffer {} has no claim-table row, so no reader could \
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

#[cfg(test)]
mod tests {
    use streamlib::sdk::iceoryx2::{
        ArrayFrameAttributeRow, ArrayFrameCpuAttributeToPack, GpuPoolSlotFrameDescriptorBody,
        array_frame_payload_of_cpu_attributes,
    };

    use super::*;

    fn gpu_attribute_slot() -> GpuPoolSlotFrameDescriptorBody {
        GpuPoolSlotFrameDescriptorBody {
            pool: *b"0123456789abcdef",
            slot: 5,
            generation: 7,
            timeline_value: 0,
            image_layout: 0,
        }
    }

    fn array_frame_of_one_gpu_attribute(
        dtype: &str,
        shape: Vec<u64>,
    ) -> (DataChannelPayloadDescriptor, Vec<u8>) {
        let row = ArrayFrameAttributeRow {
            name: "image".to_string(),
            dtype: dtype.to_string(),
            shape,
            residency: ArrayFrameAttributeResidency::GpuPoolSlot(gpu_attribute_slot()),
        };
        (
            DataChannelPayloadDescriptor::array_frame(ArrayFrameDescriptorBody {
                attribute_count: 1,
            }),
            row.to_row_bytes().expect("the row packs").to_vec(),
        )
    }

    fn no_pixel_buffer_is_acquired(
        _width: u32,
        _height: u32,
        _pixel_format: PixelFormat,
    ) -> Result<NativeNodeOutputPixelBufferFrame> {
        panic!("a GPU attribute is passed on, never copied into a pixel buffer")
    }

    #[test]
    fn a_gpu_attribute_becomes_the_image_frames_own_slot() {
        let (descriptor, payload) = array_frame_of_one_gpu_attribute("uint8", vec![16, 64, 4]);
        let image_descriptor = image_frame_of_the_array_frame(
            "image",
            TextureFormat::Bgra8Unorm,
            descriptor,
            &payload,
            no_pixel_buffer_is_acquired,
        )
        .expect("a [h, w, 4] uint8 attribute is a bgra8 image");
        assert_eq!(
            DataChannelPayloadDescriptorKind::from_wire(image_descriptor.kind),
            Some(DataChannelPayloadDescriptorKind::ImageFrame)
        );
        let image = ImageFrameDescriptorBody::from_descriptor_body(&image_descriptor.body);
        assert_eq!(image.slot, gpu_attribute_slot());
        assert_eq!((image.width, image.height), (64, 16));
        assert_eq!(image.format, TextureFormat::Bgra8Unorm as u32);
        assert_eq!(image.colour, ImageFrameH273Colour::UNSPECIFIED);
    }

    #[test]
    fn an_attribute_of_another_dtype_or_shape_is_refused() {
        for (dtype, shape) in [
            ("float32", vec![16, 64, 4]),
            ("uint8", vec![16, 64, 3]),
            ("uint8", vec![16, 64]),
            ("uint8", vec![0, 64, 4]),
        ] {
            let (descriptor, payload) = array_frame_of_one_gpu_attribute(dtype, shape.clone());
            let refusal = image_frame_of_the_array_frame(
                "image",
                TextureFormat::Rgba8Unorm,
                descriptor,
                &payload,
                no_pixel_buffer_is_acquired,
            )
            .expect_err("the attribute is no rgba8 image");
            assert!(
                refusal.to_string().contains("attribute \"image\""),
                "{dtype} {shape:?}: {refusal}"
            );
        }
    }

    #[test]
    fn a_frame_without_the_attribute_is_refused_naming_what_it_has() {
        let data = vec![0_u8; 16];
        let (body, payload) =
            array_frame_payload_of_cpu_attributes(&[ArrayFrameCpuAttributeToPack {
                name: "mask",
                dtype: "uint8",
                shape: &[2, 2, 4],
                data: &data,
            }])
            .expect("the payload packs");
        let refusal = image_frame_of_the_array_frame(
            "image",
            TextureFormat::Rgba8Unorm,
            DataChannelPayloadDescriptor::array_frame(body),
            &payload,
            no_pixel_buffer_is_acquired,
        )
        .expect_err("no attribute is named image");
        assert!(refusal.to_string().contains("[\"mask\"]"), "{refusal}");
    }

    #[test]
    fn a_format_with_no_one_tensor_layout_is_refused_at_setup() {
        for format in ["nv12", "rgb8", ""] {
            let config = ArrayToImageConfig {
                format: format.to_string(),
                ..ArrayToImageConfig::default()
            };
            assert!(image_format_of_the_config(&config).is_err(), "{format}");
        }
        assert_eq!(
            image_format_of_the_config(&ArrayToImageConfig::default()).ok(),
            Some(TextureFormat::Rgba8Unorm)
        );
    }
}

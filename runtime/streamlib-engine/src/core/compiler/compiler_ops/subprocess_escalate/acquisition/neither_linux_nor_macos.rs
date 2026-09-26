// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use uuid::Uuid;

use super::super::handle_lifecycle::{EscalateHandleRegistry, RegisteredHandle};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestAcquireImage;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::EscalateResponseErr;
use crate::core::context::{
    GpuContextFullAccess, GpuContextLimitedAccess, PooledTextureHandle, TexturePoolDescriptor,
};
use crate::core::rhi::{TextureFormat, TextureUsages};

/// Allocate one texture and register it in the parent's texture cache,
/// answering the id it is registered under and what holds it alive.
pub(super) fn allocate_registered_texture_for_helper(
    full: &GpuContextFullAccess,
    width: u32,
    height: u32,
    parsed_format: TextureFormat,
    parsed_usage: TextureUsages,
) -> crate::core::error::Result<(String, RegisteredHandle)> {
    let desc = TexturePoolDescriptor::new(width, height, parsed_format).with_usage(parsed_usage);
    let texture = full.acquire_texture(&desc)?;
    let (handle_id,) = assign_texture_handle_id(full, &texture)?;
    full.register_texture(&handle_id, texture.texture_clone());
    Ok((handle_id, RegisteredHandle::Texture { texture }))
}

pub(super) fn assign_texture_handle_id(
    _full: &crate::core::context::GpuContextFullAccess,
    _texture: &PooledTextureHandle,
) -> crate::core::error::Result<(String,)> {
    Ok((Uuid::new_v4().to_string(),))
}

pub(in super::super) fn handle_acquire_image(
    _sandbox: &GpuContextLimitedAccess,
    _registry: &EscalateHandleRegistry,
    request_id: String,
    _request: EscalateRequestAcquireImage,
) -> EscalateResponse {
    EscalateResponse::Err(EscalateResponseErr {
        request_id,
        message: "acquire_image is not available on this platform".to_string(),
    })
}

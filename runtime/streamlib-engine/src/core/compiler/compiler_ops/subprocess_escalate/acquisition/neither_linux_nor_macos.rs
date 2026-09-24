// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use uuid::Uuid;

use super::super::handle_lifecycle::EscalateHandleRegistry;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestAcquireImage;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::EscalateResponseErr;
use crate::core::context::{GpuContextLimitedAccess, PooledTextureHandle, TexturePoolDescriptor};
use crate::core::rhi::{TextureFormat, TextureUsages};

/// Acquire one texture from the pool and register it for a helper, answering
/// the handle id the helper names it by.
pub(super) fn acquire_texture_for_helper(
    sandbox: &GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
    width: u32,
    height: u32,
    parsed_format: TextureFormat,
    parsed_usage: TextureUsages,
) -> crate::core::error::Result<String> {
    sandbox.escalate(|full| {
        let desc =
            TexturePoolDescriptor::new(width, height, parsed_format).with_usage(parsed_usage);
        let texture = full.acquire_texture(&desc)?;
        let (handle_id,) = assign_texture_handle_id(full, &texture)?;
        full.register_texture(&handle_id, texture.texture_clone());
        registry.insert_texture(handle_id.clone(), texture);
        Ok(handle_id)
    })
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

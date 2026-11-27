// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib::sdk::context::RuntimeContextLimitedAccess;
use streamlib::sdk::error::Result;
use streamlib::sdk::nodes::Input;

#[streamlib::sdk::node(
    execution = continuous,
    input("threshold", description = "A number the node reads each frame.", value = f32, default = 0.0),
)]
#[meta(description = "Attribute-macro test fixture: one value input, read each frame.")]
pub fn configured_node(
    threshold: Input<f32>,
) -> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()> {
    move |_ctx| {
        let _threshold = *threshold.get();
        Ok(())
    }
}

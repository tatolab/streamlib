// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Engine-internal test fixtures shared across `#[cfg(test)]` modules.
//!
//! The TestMock processor types live here so engine tests can drive
//! graph + compiler code without depending on any external package's
//! processors. The `#[processor]` macro never auto-registers; tests
//! register the mocks explicitly via [`ensure_test_mocks_registered`].

use std::sync::Once;

use crate::core::processors::PROCESSOR_REGISTRY;

/// Mock processor with two input ports + two output ports.
#[crate::processor(
    "@tatolab/streamlib-engine/TestMockProcessor@1.0.0",
    execution = manual,
    input("in1", any),
    input("in2", any),
    output("out1", any),
    output("out2", any),
)]
pub(crate) struct MockProcessor;

impl crate::core::ManualProcessor for MockProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
    fn teardown(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
    fn start(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// Mock processor with only output ports.
#[crate::processor(
    "@tatolab/streamlib-engine/TestMockOutputOnlyProcessor@1.0.0",
    execution = manual,
    output("out1", any),
    output("out2", any),
)]
pub(crate) struct MockOutputOnlyProcessor;

impl crate::core::ManualProcessor for MockOutputOnlyProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
    fn teardown(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
    fn start(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// Mock processor with only input ports.
#[crate::processor(
    "@tatolab/streamlib-engine/TestMockInputOnlyProcessor@1.0.0",
    execution = manual,
    input("in1", any),
    input("in2", any),
)]
pub(crate) struct MockInputOnlyProcessor;

impl crate::core::ManualProcessor for MockInputOnlyProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
    fn teardown(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
    fn start(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// Register all engine-internal test mock processors with the global
/// `PROCESSOR_REGISTRY`. Idempotent — safe to call from every test
/// fixture that builds a graph against `lookup_registered_ident` or
/// drives the compiler against a `ProcessorSpec`.
pub(crate) fn ensure_test_mocks_registered() {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        PROCESSOR_REGISTRY.register::<MockProcessor::Processor>();
        PROCESSOR_REGISTRY.register::<MockOutputOnlyProcessor::Processor>();
        PROCESSOR_REGISTRY.register::<MockInputOnlyProcessor::Processor>();
    });
}

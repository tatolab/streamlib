// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Engine-internal test fixtures shared across `#[cfg(test)]` modules.
//!
//! The TestMock processor types live here so engine tests can drive
//! graph + compiler code without depending on any external package's
//! processors. They are declared with `no_inventory` so registration
//! is explicit (via [`ensure_test_mocks_registered`]) rather than
//! happening at link-time via the macro's `inventory::submit!`.

use std::sync::Once;

use crate::core::processors::PROCESSOR_REGISTRY;

/// Mock processor with two input ports + two output ports.
#[crate::processor("TestMockProcessor", no_inventory)]
pub(crate) struct MockProcessor;

impl crate::core::ManualProcessor for MockProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> impl std::future::Future<Output = crate::core::error::Result<()>> + Send {
        std::future::ready(Ok(()))
    }
    fn teardown(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> impl std::future::Future<Output = crate::core::error::Result<()>> + Send {
        std::future::ready(Ok(()))
    }
    fn start(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// Mock processor with only output ports.
#[crate::processor("TestMockOutputOnlyProcessor", no_inventory)]
pub(crate) struct MockOutputOnlyProcessor;

impl crate::core::ManualProcessor for MockOutputOnlyProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> impl std::future::Future<Output = crate::core::error::Result<()>> + Send {
        std::future::ready(Ok(()))
    }
    fn teardown(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> impl std::future::Future<Output = crate::core::error::Result<()>> + Send {
        std::future::ready(Ok(()))
    }
    fn start(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// Mock processor with only input ports.
#[crate::processor("TestMockInputOnlyProcessor", no_inventory)]
pub(crate) struct MockInputOnlyProcessor;

impl crate::core::ManualProcessor for MockInputOnlyProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> impl std::future::Future<Output = crate::core::error::Result<()>> + Send {
        std::future::ready(Ok(()))
    }
    fn teardown(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> impl std::future::Future<Output = crate::core::error::Result<()>> + Send {
        std::future::ready(Ok(()))
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

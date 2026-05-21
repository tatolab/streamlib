// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Integration-test fixture for the dlopen-owns-tokio claim from #885.
//!
//! In its `start()` lifecycle method this processor:
//!   1. Builds its own `tokio::runtime::Runtime` (`new_current_thread`,
//!      single worker).
//!   2. Calls `tokio::net::TcpListener::bind("127.0.0.1:0")` on that
//!      runtime via `block_on`.
//!   3. Writes the bound port (or an error string) to the configured
//!      `output_path` so the integration test can verify the bind
//!      succeeded even though the processor's `tokio` crate is
//!      statically linked into a dlopen'd cdylib.
//!
//! The architectural claim this proves: a plugin's own runtime is the
//! only way `tokio::net::*` futures can find their TLS slots, because
//! the host runtime's TLS lives in a different crate instance (one
//! statically linked into the host binary, one statically linked into
//! the cdylib).

use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ManualProcessor;

#[streamlib::sdk::processor("TcpBindTestProcessor")]
pub struct TcpBindTest {
    tokio_runtime: Option<tokio::runtime::Runtime>,
}

impl ManualProcessor for TcpBindTest::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| Error::Runtime(format!("TcpBindTest: build runtime: {e}")))?;
        self.tokio_runtime = Some(runtime);
        Ok(())
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let output_path = self.config.output_path.clone();
        let runtime = self
            .tokio_runtime
            .as_ref()
            .ok_or_else(|| Error::Runtime("TcpBindTest: setup() didn't build runtime".into()))?;

        let result: std::io::Result<u16> = runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            listener.local_addr().map(|addr| addr.port())
        });

        let line = match result {
            Ok(port) => port.to_string(),
            Err(e) => format!("ERR:{e}"),
        };
        std::fs::write(&output_path, &line).map_err(|e| {
            Error::Runtime(format!("TcpBindTest: write {output_path}: {e}"))
        })?;
        Ok(())
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.tokio_runtime.take();
        Ok(())
    }

    fn on_pause(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn on_resume(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }
}

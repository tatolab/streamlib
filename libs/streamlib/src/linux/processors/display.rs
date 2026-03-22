// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// TODO(#166): Vulkan + winit display implementation
// Blocked by:
//   - #178 (Cross-platform PixelFormat) — Linux PixelFormat is currently { Unknown }
//   - VK_KHR_swapchain not yet in VulkanDevice
//
// Implementation plan:
//   1. Create winit window in setup()
//   2. Create Vulkan surface from window (VK_KHR_surface + platform extension)
//   3. Create swapchain (VK_KHR_swapchain)
//   4. On frame arrival: acquire swapchain image, blit input texture to it, present
//
// Dependencies needed in Cargo.toml:
//   winit = "0.30"
//   ash-window = "0.13"
//   raw-window-handle = "0.6"
//
// Note: Headless servers have no display — see #180 risk #5.
// Hardware testing required — cannot validate without a display server (X11/Wayland).

use crate::core::{Result, RuntimeContext, StreamError};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Default)]
pub struct LinuxWindowId(pub u64);

static NEXT_WINDOW_ID: AtomicU64 = AtomicU64::new(1);

#[crate::processor("com.tatolab.display")]
pub struct LinuxDisplayProcessor {
    gpu_context: Option<crate::core::GpuContext>,
    window_id: LinuxWindowId,
    window_title: String,
    width: u32,
    height: u32,
    running: Arc<AtomicBool>,
    frame_counter: Arc<AtomicU64>,
}

impl crate::core::ManualProcessor for LinuxDisplayProcessor::Processor {
    fn setup(
        &mut self,
        ctx: RuntimeContext,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        let result = (|| {
            tracing::trace!("Display: setup() called");
            self.gpu_context = Some(ctx.gpu.clone());
            self.window_id = LinuxWindowId(NEXT_WINDOW_ID.fetch_add(1, Ordering::SeqCst));
            self.width = self.config.width;
            self.height = self.config.height;
            self.window_title = self
                .config
                .title
                .clone()
                .unwrap_or_else(|| "streamlib Display".to_string());

            self.running = Arc::new(AtomicBool::new(false));

            tracing::info!(
                "Display {}: Setup complete ({}x{}) — Vulkan rendering not yet implemented",
                self.window_title,
                self.width,
                self.height
            );

            Ok(())
        })();
        std::future::ready(result)
    }

    fn teardown(&mut self) -> impl std::future::Future<Output = Result<()>> + Send {
        tracing::info!("Display {}: Teardown", self.window_title);
        std::future::ready(Ok(()))
    }

    fn start(&mut self) -> Result<()> {
        // TODO(#166): Implement Vulkan + winit display rendering
        // Steps:
        //   1. Create winit event loop and window (on main thread or dedicated thread)
        //   2. Create VkSurfaceKHR from window handle (ash-window)
        //   3. Create VkSwapchainKHR with appropriate present mode (VSync / Mailbox)
        //   4. Spawn render thread:
        //      a. Poll inputs for Videoframe
        //      b. Acquire swapchain image
        //      c. Blit/copy input texture to swapchain image
        //      d. Present swapchain image
        //   5. Handle window resize events (recreate swapchain)
        //
        // Blocked by:
        //   - VK_KHR_swapchain not yet in VulkanDevice
        //   - #178 (Cross-platform PixelFormat)
        Err(StreamError::Configuration(
            "Linux Vulkan display not yet implemented — blocked by VK_KHR_swapchain and #178".into(),
        ))
    }

    fn stop(&mut self) -> Result<()> {
        self.running.store(false, Ordering::Release);
        tracing::info!("Display {}: Stopped", self.window_title);
        Ok(())
    }
}

impl LinuxDisplayProcessor::Processor {
    pub fn window_id(&self) -> LinuxWindowId {
        self.window_id
    }

    pub fn set_window_title(&mut self, title: &str) {
        self.window_title = title.to_string();
        // TODO(#166): Update winit window title when window is available
    }
}

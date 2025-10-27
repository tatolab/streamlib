//! Apple platform processor implementations

pub mod camera;
pub mod display;

pub use camera::AppleCameraProcessor;
pub use display::AppleDisplayProcessor;

use crate::core::{register_processor, ProcessorFactory, StreamProcessor};
use std::sync::Arc;

/// Register Apple platform processors with factories
///
/// This function must be called to enable runtime creation of CameraProcessor and DisplayProcessor.
/// It's automatically called by the global_registry() initialization.
pub(crate) fn register_apple_processors() {
    // Register CameraProcessor (aliased from AppleCameraProcessor)
    if let Some(descriptor) = AppleCameraProcessor::descriptor() {
        let factory: ProcessorFactory = Arc::new(|| {
            AppleCameraProcessor::new().map(|p| Box::new(p) as Box<dyn StreamProcessor>)
        });

        let _ = register_processor(descriptor, factory);
    }

    // Register DisplayProcessor (aliased from AppleDisplayProcessor)
    if let Some(descriptor) = AppleDisplayProcessor::descriptor() {
        let factory: ProcessorFactory = Arc::new(|| {
            AppleDisplayProcessor::new().map(|p| Box::new(p) as Box<dyn StreamProcessor>)
        });

        let _ = register_processor(descriptor, factory);
    }
}

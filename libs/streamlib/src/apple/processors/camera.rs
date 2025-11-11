
use crate::core::{
    CameraProcessor, CameraDevice,
    VideoFrame, StreamOutput, Result, StreamError,
    ProcessorDescriptor, PortDescriptor, SCHEMA_VIDEO_FRAME,
};
use crate::core::traits::{StreamElement, StreamProcessor, ElementType};
use crate::core::scheduling::{SchedulingConfig, SchedulingMode, ThreadPriority};
use streamlib_macros::StreamProcessor;
use std::sync::Arc;
use parking_lot::Mutex;
use std::ffi::c_void;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{msg_send, define_class};
use objc2_foundation::{MainThreadMarker, NSString, NSObject, NSObjectProtocol};
use objc2_av_foundation::{
    AVCaptureDevice, AVCaptureSession, AVCaptureDeviceInput, AVCaptureVideoDataOutput,
    AVMediaTypeVideo, AVCaptureVideoDataOutputSampleBufferDelegate, AVCaptureConnection,
};
use objc2_core_video::CVPixelBuffer;
use objc2_io_surface::IOSurface;
use crate::apple::{WgpuBridge, MetalDevice, iosurface};

type CMSampleBufferRef = *mut c_void;

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMSampleBufferGetImageBuffer(sbuf: CMSampleBufferRef) -> *mut CVPixelBuffer;
    fn CVPixelBufferGetIOSurface(pixelBuffer: *const CVPixelBuffer) -> *mut IOSurface;
    fn CVPixelBufferGetWidth(pixelBuffer: *const CVPixelBuffer) -> usize;
    fn CVPixelBufferGetHeight(pixelBuffer: *const CVPixelBuffer) -> usize;
}


struct FrameHolder {
    pixel_buffer: Retained<CVPixelBuffer>,
}

unsafe impl Send for FrameHolder {}
unsafe impl Sync for FrameHolder {}

static FRAME_STORAGE: std::sync::OnceLock<Arc<Mutex<Option<FrameHolder>>>> = std::sync::OnceLock::new();

static WAKEUP_CHANNEL: std::sync::OnceLock<Arc<Mutex<Option<crossbeam_channel::Sender<crate::core::runtime::WakeupEvent>>>>> = std::sync::OnceLock::new();

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "StreamlibCameraDelegate"]
    pub struct CameraDelegate;

    unsafe impl NSObjectProtocol for CameraDelegate {}

    unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for CameraDelegate {
        #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
        unsafe fn capture_output_did_output_sample_buffer_from_connection(
            &self,
            _output: *mut AnyObject,
            sample_buffer: CMSampleBufferRef,
            _connection: *mut AVCaptureConnection,
        ) {
            let pixel_buffer_ref = CMSampleBufferGetImageBuffer(sample_buffer);
            if pixel_buffer_ref.is_null() {
                eprintln!("Camera: Sample buffer has no image buffer!");
                return;
            }

            let pixel_buffer = Retained::retain(pixel_buffer_ref as *mut CVPixelBuffer)
                .expect("Failed to retain pixel buffer");

            if let Some(storage) = FRAME_STORAGE.get() {
                let frame_holder = FrameHolder { pixel_buffer: pixel_buffer.clone() };
                let mut latest = storage.lock();
                *latest = Some(frame_holder);


                if let Some(wakeup_storage) = WAKEUP_CHANNEL.get() {
                    if let Some(tx) = wakeup_storage.lock().as_ref() {
                        let _ = tx.send(crate::core::runtime::WakeupEvent::DataAvailable);
                    }
                }
            } else {
                eprintln!("Camera: FRAME_STORAGE not initialized!");
            }
        }
    }
);

impl CameraDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        unsafe {
            let this: Retained<Self> = msg_send![mtm.alloc::<Self>(), init];
            this
        }
    }
}

#[derive(StreamProcessor)]
pub struct AppleCameraProcessor {
    // Port field - annotated!
    #[output]
    video: StreamOutput<VideoFrame>,

    // Config fields
    #[allow(dead_code)] // Stored for future device management features
    device_id: Option<String>,
    frame_count: u64,

    latest_frame: Arc<Mutex<Option<FrameHolder>>>,

    gpu_context: Option<crate::core::GpuContext>,

    metal_device: Option<MetalDevice>,

    wgpu_bridge: Option<Arc<WgpuBridge>>,

    #[allow(dead_code)] // Stored for future logging/diagnostics
    camera_name: String,

    #[allow(dead_code)]
    delegate: Option<Retained<CameraDelegate>>,

    metal_command_queue: Option<metal::CommandQueue>,
}

impl AppleCameraProcessor {
    pub fn new() -> Result<Self> {
        Self::with_device_id_opt(None)
    }

    pub fn with_device_id(device_id: &str) -> Result<Self> {
        Self::with_device_id_opt(Some(device_id))
    }

    fn with_device_id_opt(device_id: Option<&str>) -> Result<Self> {

        let mtm = MainThreadMarker::new()
            .ok_or_else(|| StreamError::Configuration(
                "CameraProcessor must be created on main thread".into()
            ))?;

        tracing::info!("Camera: Initializing AVFoundation capture session");

        let metal_device = MetalDevice::new()?;

        let latest_frame = Arc::new(Mutex::new(None));

        let session = unsafe { AVCaptureSession::new() };

        unsafe {
            session.beginConfiguration();
        }

        let device = unsafe {
            if let Some(id) = device_id {
                let id_str = NSString::from_str(id);
                let dev = AVCaptureDevice::deviceWithUniqueID(&id_str);
                if dev.is_none() {
                    return Err(StreamError::Configuration(
                        format!("Camera not found with ID: {}. The device may have been disconnected or the ID changed.", id)
                    ));
                }
                dev.unwrap()
            } else {
                let media_type = AVMediaTypeVideo.ok_or_else(|| StreamError::Configuration(
                    "AVMediaTypeVideo not available".into()
                ))?;

                AVCaptureDevice::defaultDeviceWithMediaType(media_type)
                    .ok_or_else(|| StreamError::Configuration(
                        "No camera found".into()
                    ))?
            }
        };

        let device_name = unsafe { device.localizedName().to_string() };
        let _device_unique_id = unsafe { device.uniqueID().to_string() };
        let device_model = unsafe { device.modelID().to_string() };
        let _device_manufacturer = unsafe { device.manufacturer().to_string() };

        tracing::info!("Camera: Found device: {} ({})", device_name, device_model);

        let media_type = unsafe {
            AVMediaTypeVideo.ok_or_else(|| StreamError::Configuration(
                "AVMediaTypeVideo not available".into()
            ))
        }?;

        let _status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media_type) };


        unsafe {
            if let Err(e) = device.lockForConfiguration() {
                eprintln!("Camera: Failed to lock device: {:?}", e);
                return Err(StreamError::Configuration(
                    format!("Failed to lock camera device: {:?}", e)
                ));
            }
            device.unlockForConfiguration();
        }

        let input = unsafe {
            AVCaptureDeviceInput::deviceInputWithDevice_error(&device)
                .map_err(|e| StreamError::Configuration(
                    format!("Failed to create camera input: {:?}", e)
                ))?
        };

        let can_add = unsafe { session.canAddInput(&input) };

        if !can_add {
            return Err(StreamError::Configuration(
                "Session cannot add camera input. The camera may be in use by another application.".into()
            ));
        }

        unsafe {
            session.addInput(&input);
        }

        let _ = FRAME_STORAGE.set(latest_frame.clone());

        let wakeup_holder: Arc<Mutex<Option<crossbeam_channel::Sender<crate::core::runtime::WakeupEvent>>>> =
            Arc::new(Mutex::new(None));
        let _ = WAKEUP_CHANNEL.set(wakeup_holder.clone());

        let output = unsafe { AVCaptureVideoDataOutput::new() };


        use objc2_foundation::NSNumber;

        let pixel_format_key = unsafe {
            objc2_core_video::kCVPixelBufferPixelFormatTypeKey
        };
        let pixel_format_value = NSNumber::new_u32(0x42475241); // BGRA

        use objc2::ClassType;
        use objc2::runtime::AnyClass;
        let dict_cls: &AnyClass = objc2_foundation::NSDictionary::<objc2::runtime::AnyObject, objc2::runtime::AnyObject>::class();

        let key_ptr = pixel_format_key as *const _ as *const objc2::runtime::AnyObject;
        let value_ptr = &*pixel_format_value as *const _ as *const objc2::runtime::AnyObject;

        let video_settings_ptr: *mut objc2::runtime::AnyObject = unsafe {
            msg_send![dict_cls, dictionaryWithObject: value_ptr, forKey: key_ptr]
        };

        unsafe {
            let _: () = msg_send![&output, setVideoSettings: video_settings_ptr];
        }

        let delegate = CameraDelegate::new(mtm);

        unsafe {
            use dispatch2::{DispatchQueue, DispatchQueueAttr};
            let queue = DispatchQueue::new(
                "com.streamlib.camera.video",
                DispatchQueueAttr::SERIAL,
            );

            output.setSampleBufferDelegate_queue(
                Some(ProtocolObject::from_ref(&*delegate)),
                Some(&queue),
            );
        }

        let can_add_output = unsafe { session.canAddOutput(&output) };

        if !can_add_output {
            return Err(StreamError::Configuration("Cannot add camera output".into()));
        }

        unsafe {
            session.addOutput(&output);
        }

        let camera_name = unsafe { device.localizedName().to_string() };

        unsafe {
            session.commitConfiguration();
        }

        tracing::info!("Camera: Starting capture session");
        unsafe { session.startRunning(); }

        // TODO: Properly manage session lifecycle
        std::mem::forget(session);

        tracing::info!("Camera: AVFoundation session running (will capture frames)");

        Ok(Self {
            // Port
            video: StreamOutput::new("video"),

            // Config fields
            device_id: device_id.map(String::from),
            frame_count: 0,
            latest_frame,
            gpu_context: None,  // Will be set by runtime in on_start()
            metal_device: Some(metal_device),
            wgpu_bridge: None,  // Will be created from shared device in on_start()
            camera_name,
            delegate: Some(delegate),
            metal_command_queue: None,  // Will be created in on_start()
        })
    }
}

impl CameraProcessor for AppleCameraProcessor {
    fn set_device_id(&mut self, _device_id: &str) -> Result<()> {
        Err(StreamError::Configuration(
            "Cannot change device after creation. Use with_device_id()".into()
        ))
    }

    fn list_devices() -> Result<Vec<CameraDevice>> {
        unsafe {
            use objc2_av_foundation::AVCaptureDeviceDiscoverySession;
            use objc2_foundation::NSArray;

            let media_type = AVMediaTypeVideo.ok_or_else(|| StreamError::Configuration(
                "AVMediaTypeVideo not available".into()
            ))?;

            let builtin_wide = objc2_foundation::ns_string!("AVCaptureDeviceTypeBuiltInWideAngleCamera");
            let continuity = objc2_foundation::ns_string!("AVCaptureDeviceTypeContinuityCamera");

            let device_types = NSArray::from_slice(&[builtin_wide, continuity]);

            let session = AVCaptureDeviceDiscoverySession::discoverySessionWithDeviceTypes_mediaType_position(
                &device_types,
                Some(media_type),
                objc2_av_foundation::AVCaptureDevicePosition::Unspecified,
            );

            let devices = session.devices();
            let mut result = Vec::new();
            for i in 0..devices.count() {
                let device = devices.objectAtIndex(i);
                result.push(CameraDevice {
                    id: device.uniqueID().to_string(),
                    name: device.localizedName().to_string(),
                });
            }

            Ok(result)
        }
    }

}


impl StreamElement for AppleCameraProcessor {
    fn name(&self) -> &str {
        &self.camera_name
    }

    fn element_type(&self) -> ElementType {
        ElementType::Source
    }

    fn descriptor(&self) -> Option<ProcessorDescriptor> {
        <AppleCameraProcessor as StreamProcessor>::descriptor()
    }

    fn input_ports(&self) -> Vec<PortDescriptor> {
        Vec::new() // Sources have no inputs
    }

    fn output_ports(&self) -> Vec<PortDescriptor> {
        vec![PortDescriptor {
            name: "video".to_string(),
            schema: SCHEMA_VIDEO_FRAME.clone(),
            required: true,
            description: "Live video frames from the camera. Each frame is a WebGPU texture with timestamp and metadata.".to_string(),
        }]
    }

    fn start(&mut self, _ctx: &crate::core::RuntimeContext) -> Result<()> {
        tracing::info!("Camera {}: Starting (AVFoundation session already running)", self.camera_name);
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        tracing::info!("Camera {}: Stopping (generated {} frames)", self.camera_name, self.frame_count);
        Ok(())
    }
}

impl StreamProcessor for AppleCameraProcessor {
    type Config = crate::core::CameraConfig;

    fn from_config(config: Self::Config) -> Result<Self> {
        match config.device_id {
            Some(device_id) => Self::with_device_id(&device_id),
            None => Self::new(),
        }
    }

    fn process(&mut self) -> Result<()> {
        let frame_holder = {
            let mut latest = self.latest_frame.lock();
            latest.take() // Take ownership, leaving None
        };

        let holder = frame_holder.ok_or_else(|| {
            StreamError::Runtime("No frame available from camera callback".into())
        })?;

        unsafe {
            let pixel_buffer_ref = &*holder.pixel_buffer as *const CVPixelBuffer;

            let iosurface_ref = CVPixelBufferGetIOSurface(pixel_buffer_ref);
            if iosurface_ref.is_null() {
                return Err(StreamError::GpuError("Frame has no IOSurface backing".into()));
            }

            let iosurface = Retained::retain(iosurface_ref)
                .expect("Failed to retain IOSurface");

            let width = CVPixelBufferGetWidth(pixel_buffer_ref);
            let height = CVPixelBufferGetHeight(pixel_buffer_ref);

            let metal_device = self.metal_device.as_ref()
                .ok_or_else(|| StreamError::Configuration("No Metal device".into()))?;

            let metal_texture = iosurface::create_metal_texture_from_iosurface(
                metal_device.device(),
                &iosurface,
                0, // plane 0 for BGRA
            )?;

            let wgpu_bridge = self.wgpu_bridge.as_ref()
                .ok_or_else(|| StreamError::Configuration("No WebGPU bridge".into()))?;

            let _iosurface_texture = wgpu_bridge.wrap_metal_texture(
                &metal_texture,
                wgpu::TextureFormat::Bgra8Unorm,
                wgpu::TextureUsages::COPY_SRC,
            )?;

            let metal_rgba_texture = {
                use objc2_metal::{MTLTextureDescriptor, MTLPixelFormat, MTLTextureUsage, MTLDevice};

                let desc = MTLTextureDescriptor::new();
                desc.setPixelFormat(MTLPixelFormat::RGBA8Unorm);
                desc.setWidth(width);
                desc.setHeight(height);
                desc.setUsage(MTLTextureUsage::ShaderRead | MTLTextureUsage::RenderTarget);

                metal_device.device().newTextureWithDescriptor(&desc)
                    .ok_or_else(|| StreamError::GpuError("Failed to create RGBA texture".into()))?
            };

            let command_queue = self.metal_command_queue.as_ref()
                .ok_or_else(|| StreamError::Configuration("Metal command queue not initialized".into()))?;

            use metal::foreign_types::ForeignTypeRef;

            let source_texture_ptr = &*metal_texture as *const _ as *mut std::ffi::c_void;
            let source_texture_ref = metal::TextureRef::from_ptr(source_texture_ptr as *mut _);

            let dest_texture_ptr = &*metal_rgba_texture as *const _ as *mut std::ffi::c_void;
            let dest_texture_ref = metal::TextureRef::from_ptr(dest_texture_ptr as *mut _);

            let command_buffer = command_queue.new_command_buffer();
            let blit_encoder = command_buffer.new_blit_command_encoder();

            use metal::MTLOrigin;
            use metal::MTLSize;

            let origin = MTLOrigin { x: 0, y: 0, z: 0 };
            let size = MTLSize {
                width: width as u64,
                height: height as u64,
                depth: 1,
            };

            blit_encoder.copy_from_texture(
                source_texture_ref,
                0,
                0,
                origin,
                size,
                dest_texture_ref,
                0,
                0,
                origin,
            );

            blit_encoder.end_encoding();
            command_buffer.commit();
            command_buffer.wait_until_completed();

            let output_texture = wgpu_bridge.wrap_metal_texture(
                &metal_rgba_texture,
                wgpu::TextureFormat::Rgba8Unorm,
                wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
            )?;

            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs_f64();

            let frame = VideoFrame::new(
                Arc::new(output_texture),
                wgpu::TextureFormat::Rgba8Unorm,
                timestamp,
                self.frame_count,
                width as u32,
                height as u32,
            );

            self.frame_count += 1;

            if self.frame_count.is_multiple_of(60) {
                tracing::info!(
                    "Camera: Generated frame {} ({}x{}) - WebGPU texture, format=Rgba8Unorm",
                    self.frame_count,
                    width,
                    height
                );
            }

            self.video.write(frame);
            Ok(())
        }
    }

    fn scheduling_config(&self) -> SchedulingConfig {
        SchedulingConfig {
            mode: SchedulingMode::Pull,
            priority: ThreadPriority::High,
        }
    }

    fn descriptor() -> Option<ProcessorDescriptor> where Self: Sized {
        Some(
            ProcessorDescriptor::new(
                "AppleCameraProcessor",
                "Captures video from macOS cameras using AVFoundation with zero-copy Metal/IOSurface integration"
            )
            .with_usage_context(
                "Automatically uses default camera if device_id not specified. \
                 Outputs GPU textures via Metal/IOSurface for zero-copy processing. \
                 Runs at native camera frame rate (typically 30 or 60 fps)."
            )
            .with_tags(vec!["video", "source", "camera", "avfoundation", "metal", "macos"])
        )
    }

    fn set_output_wakeup(&mut self, port_name: &str, wakeup_tx: crossbeam_channel::Sender<crate::core::runtime::WakeupEvent>) {
        if port_name == "video" {
            self.video.set_downstream_wakeup(wakeup_tx);
        }
    }
}

crate::register_processor_type!(AppleCameraProcessor);

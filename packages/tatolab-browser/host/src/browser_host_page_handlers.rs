// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The CEF handlers of one tab's page: its paints, audio, load state, published
//! objects and renderer's end, each sent on to the tab's node.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use cef::rc::Rc as _;
use cef::*;
use tatolab_browser_page_bridge::PAGE_PUBLISH_PROCESS_MESSAGE_NAME;
use tatolab_browser_tab_protocol::{BrowserHostTabLogLevel, TabProtocolMessageFromBrowserHost};

use crate::browser_host_cef_thread::{CefThreadRequest, CefThreadRequestSender};
use crate::browser_host_tab::{BrowserHostTab, PaintedTextureOfThePage};

/// The CEF client of `tab`'s page.
pub fn browser_host_page_client(
    tab: &Arc<BrowserHostTab>,
    cef_thread_requests: &CefThreadRequestSender,
) -> Client {
    PageClient::new(
        PageRenderHandler::new(Arc::clone(tab)),
        PageAudioHandler::new(Arc::clone(tab)),
        PageLifeSpanHandler::new(Arc::clone(tab)),
        PageLoadHandler::new(Arc::clone(tab)),
        PageRequestHandler::new(Arc::clone(tab), cef_thread_requests.clone()),
        Arc::clone(tab),
    )
}

wrap_render_handler! {
    struct PageRenderHandler {
        tab: Arc<BrowserHostTab>,
    }

    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            if let Some(rect) = rect {
                *rect = Rect {
                    x: 0,
                    y: 0,
                    width: self.tab.tab_request.width as i32,
                    height: self.tab.tab_request.height as i32,
                };
            }
        }

        fn on_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            buffer: *const u8,
            width: ::std::os::raw::c_int,
            height: ::std::os::raw::c_int,
        ) {
            if type_ != PaintElementType::VIEW || buffer.is_null() || width <= 0 || height <= 0 {
                return;
            }
            let (width, height) = (width as u32, height as u32);
            // SAFETY: CEF hands `width × height` packed BGRA pixels, alive for
            // the length of this call.
            let painted_bgra_rows = unsafe {
                std::slice::from_raw_parts(buffer, width as usize * height as usize * 4)
            };
            self.tab.hand_a_painted_frame_to_the_node(painted_bgra_rows, width, height);
        }

        fn on_accelerated_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            info: Option<&AcceleratedPaintInfo>,
        ) {
            if type_ != PaintElementType::VIEW {
                return;
            }
            let Some(info) = info else {
                return;
            };
            if let Some(painted) = painted_texture_of_the_page(info, &self.tab) {
                self.tab.hand_a_painted_texture_to_the_node(painted);
            }
        }
    }
}

/// The pixel format wire name of what CEF painted, or `None` for one the node
/// cannot import.
fn painted_pixel_format_of(info: &AcceleratedPaintInfo) -> Option<&'static str> {
    if info.format == ColorType::BGRA_8888 {
        Some("bgra32")
    } else if info.format == ColorType::RGBA_8888 {
        Some("rgba32")
    } else {
        None
    }
}

/// The IOSurface `info` names, at the tab's extent, or `None` when CEF
/// painted something the node cannot import; the runtime refuses one smaller
/// than the extent.
#[cfg(target_os = "macos")]
fn painted_texture_of_the_page(
    info: &AcceleratedPaintInfo,
    tab: &BrowserHostTab,
) -> Option<PaintedTextureOfThePage> {
    let pixel_format = painted_pixel_format_of(info)?;
    if info.shared_texture_io_surface.is_null() {
        return None;
    }
    Some(PaintedTextureOfThePage {
        width: tab.tab_request.width,
        height: tab.tab_request.height,
        pixel_format,
        iosurface: info.shared_texture_io_surface.cast_const(),
    })
}

/// The texture `info` describes, cut to the tab's extent, or `None` when CEF
/// painted something the node cannot import.
#[cfg(target_os = "linux")]
fn painted_texture_of_the_page(
    info: &AcceleratedPaintInfo,
    tab: &BrowserHostTab,
) -> Option<PaintedTextureOfThePage> {
    let pixel_format = painted_pixel_format_of(info)?;
    let plane_count = usize::try_from(info.plane_count).ok()?;
    let planes = info
        .planes
        .get(..plane_count)
        .filter(|planes| !planes.is_empty())?;
    let (width, height) = (tab.tab_request.width, tab.tab_request.height);
    let coded_height = u64::try_from(info.extra.coded_size.height).ok()?;
    if i64::from(info.extra.coded_size.width) < i64::from(width) || coded_height < u64::from(height)
    {
        return None;
    }
    let mut allocation_byte_size = 0;
    for plane in planes {
        let plane_end = u64::from(plane.stride)
            .checked_mul(coded_height)?
            .checked_add(plane.offset)?;
        allocation_byte_size = allocation_byte_size.max(plane.size).max(plane_end);
    }
    Some(PaintedTextureOfThePage {
        width,
        height,
        pixel_format,
        plane_descriptors: Vec::from_iter(planes.iter().map(|plane| plane.fd)),
        plane_offsets_bytes: Vec::from_iter(planes.iter().map(|plane| plane.offset)),
        plane_strides_bytes: Vec::from_iter(planes.iter().map(|plane| u64::from(plane.stride))),
        allocation_byte_size,
        drm_format_modifier: info.modifier,
    })
}

wrap_audio_handler! {
    struct PageAudioHandler {
        tab: Arc<BrowserHostTab>,
    }

    impl AudioHandler {
        fn audio_parameters(
            &self,
            _browser: Option<&mut Browser>,
            params: Option<&mut AudioParameters>,
        ) -> ::std::os::raw::c_int {
            let Some(params) = params else {
                return 0;
            };
            let sample_rate = self.tab.tab_request.audio_sample_rate;
            params.sample_rate = sample_rate as i32;
            params.channel_layout = ChannelLayout::LAYOUT_STEREO;
            params.frames_per_buffer = (sample_rate / 100).max(1) as i32;
            1
        }

        fn on_audio_stream_started(
            &self,
            _browser: Option<&mut Browser>,
            params: Option<&AudioParameters>,
            channels: ::std::os::raw::c_int,
        ) {
            let sample_rate = params
                .map(|params| params.sample_rate.max(1) as u32)
                .unwrap_or(self.tab.tab_request.audio_sample_rate);
            self.tab.audio_stream_started(sample_rate, channels.max(1) as u32);
        }

        fn on_audio_stream_packet(
            &self,
            _browser: Option<&mut Browser>,
            data: *mut *const f32,
            frames: ::std::os::raw::c_int,
            _pts: i64,
        ) {
            let channels = self.tab.audio_stream_channels() as usize;
            if data.is_null() || frames <= 0 || channels == 0 {
                return;
            }
            // SAFETY: CEF hands one plane pointer per channel of the stream
            // it started, each `frames` samples long, alive for this call.
            let channel_planes: Vec<&[f32]> = (0..channels)
                .map(|channel| unsafe {
                    std::slice::from_raw_parts(*data.add(channel), frames as usize)
                })
                .collect();
            self.tab.hand_an_audio_packet_to_the_node(&channel_planes, frames as usize);
        }

        fn on_audio_stream_stopped(&self, _browser: Option<&mut Browser>) {
            self.tab.audio_stream_stopped();
        }

        fn on_audio_stream_error(&self, _browser: Option<&mut Browser>, message: Option<&CefString>) {
            self.tab.audio_stream_stopped();
            self.tab.log_to_the_node(
                BrowserHostTabLogLevel::Warn,
                format!(
                    "the page's audio stream failed: {}",
                    message.map(ToString::to_string).unwrap_or_default()
                ),
            );
        }
    }
}

wrap_life_span_handler! {
    struct PageLifeSpanHandler {
        tab: Arc<BrowserHostTab>,
    }

    impl LifeSpanHandler {
        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            self.tab.page_closed.store(true, Ordering::Release);
        }
    }
}

wrap_load_handler! {
    struct PageLoadHandler {
        tab: Arc<BrowserHostTab>,
    }

    impl LoadHandler {
        fn on_load_start(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _transition_type: TransitionType,
        ) {
            if frame.is_some_and(|frame| frame.is_main() != 0) {
                self.tab.main_frame_document_loaded.store(false, Ordering::Release);
            }
        }

        fn on_load_end(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _http_status_code: ::std::os::raw::c_int,
        ) {
            if frame.is_some_and(|frame| frame.is_main() != 0) {
                self.tab.main_frame_document_loaded.store(true, Ordering::Release);
            }
        }

        fn on_load_error(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _error_code: Errorcode,
            error_text: Option<&CefString>,
            failed_url: Option<&CefString>,
        ) {
            if frame.is_some_and(|frame| frame.is_main() != 0) {
                self.tab.log_to_the_node(
                    BrowserHostTabLogLevel::Warn,
                    format!(
                        "the page did not load {}: {}",
                        failed_url.map(ToString::to_string).unwrap_or_default(),
                        error_text.map(ToString::to_string).unwrap_or_default()
                    ),
                );
            }
        }
    }
}

wrap_request_handler! {
    struct PageRequestHandler {
        tab: Arc<BrowserHostTab>,
        cef_thread_requests: CefThreadRequestSender,
    }

    impl RequestHandler {
        fn on_render_process_terminated(
            &self,
            _browser: Option<&mut Browser>,
            status: TerminationStatus,
            error_code: ::std::os::raw::c_int,
            error_string: Option<&CefString>,
        ) {
            self.tab.main_frame_document_loaded.store(false, Ordering::Release);
            self.tab.log_to_the_node(
                BrowserHostTabLogLevel::Warn,
                format!(
                    "the renderer of {} ended ({status:?}, code {error_code}: {})",
                    self.tab.tab_request.url,
                    error_string.map(ToString::to_string).unwrap_or_default()
                ),
            );
            self.cef_thread_requests.send(CefThreadRequest::ReloadTabAfterItsRendererEnded {
                tab_id: self.tab.tab_id,
            });
        }
    }
}

wrap_client! {
    struct PageClient {
        render_handler: RenderHandler,
        audio_handler: AudioHandler,
        life_span_handler: LifeSpanHandler,
        load_handler: LoadHandler,
        request_handler: RequestHandler,
        tab: Arc<BrowserHostTab>,
    }

    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.render_handler.clone())
        }

        fn audio_handler(&self) -> Option<AudioHandler> {
            Some(self.audio_handler.clone())
        }

        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.life_span_handler.clone())
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            Some(self.load_handler.clone())
        }

        fn request_handler(&self) -> Option<RequestHandler> {
            Some(self.request_handler.clone())
        }

        fn on_process_message_received(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> ::std::os::raw::c_int {
            let Some(message) = message else {
                return 0;
            };
            if CefString::from(&message.name()).to_string() != PAGE_PUBLISH_PROCESS_MESSAGE_NAME {
                return 0;
            }
            if let Some(arguments) = message.argument_list() {
                let published_json = CefString::from(&arguments.string(0)).to_string();
                self.tab.send_to_the_node(&TabProtocolMessageFromBrowserHost::PagePublished {
                    published_json,
                });
            }
            1
        }
    }
}

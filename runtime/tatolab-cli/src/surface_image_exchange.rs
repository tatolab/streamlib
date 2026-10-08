// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab exchange`: published surface ids in, exact PNG files on disk out. The id form is one
//! exchange. The channel form taps a channel, reads a surface id out of each sampled bag here, and
//! exchanges it; the runtime is never asked to read a bag.
//!
//! Either form runs on one local API connection: the channel form's tap rounds and exchanges share
//! its MCP client and its HTTP/1.1 connection, so a sampled frame is exchanged while its pool slot
//! still holds it rather than after a connect per frame. A frame whose slot was recycled first is
//! retried against a newer bag and reported, so a short sample never reads as a full one.

use std::ffi::OsString;
use std::fmt::Write;
use std::path::{Component, Path, PathBuf};

use clap::Args;
use hyper::body::Bytes;
use hyper::{HeaderMap, StatusCode};
use streamlib_ipc_types::{FrameHeader, TappedFramePayloadRefusal};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    RECYCLED_FRAME_HTTP_STATUS_CODE, SURFACE_PIXEL_HEIGHT_HEADER_NAME,
    SURFACE_PIXEL_WIDTH_HEADER_NAME, TapToolResult,
    surface_image_exchange_route_path_for_surface_id,
};

use crate::local_api_connection::LocalApiConnection;
use crate::local_api_mcp_tool_client::{
    LocalApiMcpToolClientFailure, OBSERVATION_VERB_TOOL_CALL_TIMEOUT,
    tool_call_failure_worded_as_an_observation_verb_reports_it,
};
use crate::local_api_runtime_selection::select_live_runtime_on_this_machine;
use crate::local_api_unix_socket_http_client::LocalApiHttpRequestFailure;
use crate::runtime_observation_verbs::{TAP_TOOL_NAME, tap_tool_arguments};
use crate::{RuntimeTargetArguments, TatolabCommandFailure};

/// The bag field the channel form reads a surface id from unless `--field` names another. The
/// runtime inspects no bag content, so which field carries an id is the caller's knowledge.
pub(crate) const DEFAULT_SURFACE_ID_BAG_FIELD_NAME: &str = "surface_id";

/// Tap rounds one channel-form run spends before giving up, so a channel whose frames always
/// recycle before their exchange cannot retry forever.
pub(crate) const MAX_TAP_ROUNDS_PER_SAMPLE_RUN: u32 = 8;

/// The file-name stem of an empty surface id.
const FILE_NAME_STEM_OF_AN_EMPTY_SURFACE_ID: &str = "surface";

/// `tatolab exchange`'s arguments: SURFACE_ID, or `--channel` with its sampling bounds.
#[derive(Args, Debug, Clone)]
pub(crate) struct SurfaceImageExchangeArguments {
    /// A surface id a bag published, e.g. `{slot}#{generation}`.
    #[arg(value_name = "SURFACE_ID")]
    pub(crate) published_surface_id: Option<String>,
    /// Directory the PNGs are written into (created when absent).
    #[arg(long = "out", value_name = "DIR")]
    pub(crate) output_directory: OsString,
    /// Sample this channel instead of naming one id: an output port's address,
    /// `<runtime_name>/<node>/<port>`.
    #[arg(long = "channel", value_name = "CHANNEL")]
    pub(crate) channel: Option<String>,
    /// (--channel only) Frames to exchange before returning. Default 1.
    #[arg(long = "count", value_name = "N", allow_negative_numbers = true)]
    pub(crate) requested_frame_count: Option<i64>,
    /// (--channel only) Exchange every Nth sampled bag. Default 1.
    #[arg(long = "every", value_name = "N", allow_negative_numbers = true)]
    pub(crate) requested_every_nth_bag: Option<i64>,
    /// (--channel only) Bag field carrying the surface id (default: surface_id).
    #[arg(long = "field", value_name = "NAME")]
    pub(crate) requested_surface_id_bag_field_name: Option<String>,
    #[command(flatten)]
    pub(crate) runtime_target: RuntimeTargetArguments,
}

/// What one `tatolab exchange` asks for, its usage checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SurfaceImageExchangeForm {
    /// One published surface id, exchanged once.
    OnePublishedSurfaceId { published_surface_id: String },
    /// Surface ids read out of a channel's sampled bags.
    SampledChannel {
        channel: String,
        sampled_channel_exchange_bounds: SampledChannelExchangeBounds,
    },
}

/// How a channel-form run samples.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SampledChannelExchangeBounds {
    /// Frames to write before returning.
    pub(crate) wanted_image_count: usize,
    /// The stride over received bags: every Nth one is selected.
    pub(crate) every_nth_bag: usize,
    /// The bag field a selected bag's surface id is read from.
    pub(crate) surface_id_bag_field_name: String,
}

/// One frame's PNG bytes, and the extent of the surface they came from when the runtime stated it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExchangedSurfaceImage {
    /// The exact image, as written to disk.
    pub(crate) png_image_bytes: Bytes,
    /// The source surface's width.
    pub(crate) source_surface_pixel_width: Option<u32>,
    /// The source surface's height.
    pub(crate) source_surface_pixel_height: Option<u32>,
}

/// Why an exchange wrote no image.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SurfaceImageExchangeFailure {
    /// The exchange request got no answer.
    #[error(transparent)]
    LocalApiRequestFailed(#[from] LocalApiHttpRequestFailure),
    /// The runtime answered and refused, with the status it used and the reason it gave.
    #[error(
        "exchange of surface `{published_surface_id}` answered {}{}",
        .http_status.as_u16(),
        refusal_detail_after_a_colon(.refusal_detail)
    )]
    RefusedByTheRuntime {
        published_surface_id: String,
        http_status: StatusCode,
        refusal_detail: String,
    },
    /// The image came back and could not be written into the output directory.
    #[error("could not write into `{}`: {write_failure}", .output_directory.display())]
    OutputDirectoryNotWritable {
        output_directory: PathBuf,
        #[source]
        write_failure: std::io::Error,
    },
}

impl SurfaceImageExchangeFailure {
    /// Whether the id named a frame whose pool slot has since been reused — the one refusal a
    /// newer bag answers. Every other refusal answers the same forever.
    pub(crate) fn names_a_recycled_frame(&self) -> bool {
        matches!(
            self,
            Self::RefusedByTheRuntime { http_status, .. }
                if http_status.as_u16() == RECYCLED_FRAME_HTTP_STATUS_CODE
        )
    }
}

/// `: <refusal_detail>`, or nothing when the runtime gave no reason.
fn refusal_detail_after_a_colon(refusal_detail: &str) -> String {
    if refusal_detail.is_empty() {
        String::new()
    } else {
        format!(": {refusal_detail}")
    }
}

impl From<SurfaceImageExchangeFailure> for TatolabCommandFailure {
    fn from(surface_image_exchange_failure: SurfaceImageExchangeFailure) -> Self {
        TatolabCommandFailure::refused(surface_image_exchange_failure.to_string())
    }
}

/// What one channel-form run exchanged, what it retried, and why it stopped early if it did.
#[derive(Debug, Default)]
pub(crate) struct SampledChannelExchangeReport {
    /// Every PNG the run wrote, in the order it wrote them.
    pub(crate) written_image_paths: Vec<PathBuf>,
    /// Ids whose frames were recycled before their exchange, each retried against a newer bag.
    pub(crate) retried_recycled_surface_ids: Vec<String>,
    /// Selected bags whose named field held no string.
    pub(crate) bags_missing_the_surface_id_field: usize,
    /// Bags received across every tap round, selected by the stride or not.
    pub(crate) bags_examined: usize,
    /// Tap rounds started.
    pub(crate) tap_rounds: u32,
    /// A failure the run could not compose past. It is reported beside the frames that landed
    /// rather than instead of them: a PNG on disk whose path was never printed is evidence nobody
    /// can use.
    pub(crate) stopped_early_because: Option<SampledChannelExchangeStop>,
}

/// Why a channel-form run stopped before it wrote every frame it wanted.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SampledChannelExchangeStop {
    /// The tap failed, was refused, or got no answer.
    #[error(transparent)]
    TapCallFailed(#[from] LocalApiMcpToolClientFailure),
    /// The tap answered with something other than the tap tool's result.
    #[error(
        "tap of `{channel}` returned a result that is not the tap tool's ({shape_failure}): \
         {tap_tool_result_text}"
    )]
    TapResultIsNotTheTapToolsShape {
        channel: String,
        tap_tool_result_text: String,
        #[source]
        shape_failure: serde_json::Error,
    },
    /// A tapped bag's hex preview does not decode to bytes.
    #[error("tap of `{channel}` returned a bag whose hex preview does not decode: {hex_failure}")]
    TappedBagHexPreviewDoesNotDecode {
        channel: String,
        #[source]
        hex_failure: hex::FromHexError,
    },
    /// A selected bag is longer than the prefix `tap` previews, so its id cannot be read from
    /// here, while the id form still reaches its frame.
    #[error(
        "a bag the sample selected on `{channel}` is {whole_bag_byte_len} bytes, past the prefix \
         `tap` previews, so its surface id cannot be read from here. Exchange an id from this \
         channel directly: `tatolab exchange <surface-id> --out <dir>`."
    )]
    SelectedBagPastTheTapPreviewCap {
        channel: String,
        whole_bag_byte_len: u64,
    },
    /// A selected bag does not decode to a msgpack value.
    #[error("a bag from `{channel}` could not be decoded: {decode_failure}")]
    SelectedBagDoesNotDecode {
        channel: String,
        #[source]
        decode_failure: TappedChannelBagDecodeFailure,
    },
    /// An exchange the runtime refused for good, or an image that could not be written.
    #[error(transparent)]
    SurfaceImageExchangeFailed(#[from] SurfaceImageExchangeFailure),
}

/// One bag a tap forwarded. Whether its capped preview matters depends on whether the stride
/// selects it, so the cap rides with the bag rather than failing its round.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TappedChannelBagFrame {
    framed_bag_bytes: Vec<u8>,
    preview_was_capped: bool,
    whole_bag_byte_len: u64,
}

/// Why a tapped bag's bytes do not decode to a msgpack value.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TappedChannelBagDecodeFailure {
    /// The bytes hold no whole frame payload.
    #[error(transparent)]
    FramePayloadRefused(#[from] TappedFramePayloadRefusal),
    /// The payload is not msgpack.
    #[error(transparent)]
    PayloadIsNotMsgpack(#[from] rmpv::decode::Error),
}

/// `tatolab exchange`: check the usage, pick the runtime, and exchange.
pub(crate) fn run_surface_image_exchange_verb(
    exchange_arguments: &SurfaceImageExchangeArguments,
) -> Result<u8, TatolabCommandFailure> {
    let exchange_form = surface_image_exchange_form(exchange_arguments)?;
    let output_directory = output_directory_without_current_directory_components(Path::new(
        &exchange_arguments.output_directory,
    ));
    let selected_runtime = select_live_runtime_on_this_machine(
        exchange_arguments
            .runtime_target
            .requested_runtime_name_or_id
            .as_deref(),
    )?;
    let mut local_api_connection = LocalApiConnection::open(
        &selected_runtime.local_api_socket_path,
        OBSERVATION_VERB_TOOL_CALL_TIMEOUT,
    )?;
    match exchange_form {
        SurfaceImageExchangeForm::OnePublishedSurfaceId {
            published_surface_id,
        } => {
            let written_image_path = exchange_one_published_surface_id_into_directory(
                &mut local_api_connection,
                &published_surface_id,
                &output_directory,
            )?;
            println!("{}", written_image_path.display());
            Ok(0)
        }
        SurfaceImageExchangeForm::SampledChannel {
            channel,
            sampled_channel_exchange_bounds,
        } => {
            let sampled_channel_exchange_report = sample_channel_into_exchanged_surface_images(
                &mut local_api_connection,
                &channel,
                &output_directory,
                &sampled_channel_exchange_bounds,
            );
            for written_image_path in &sampled_channel_exchange_report.written_image_paths {
                println!("{}", written_image_path.display());
            }
            eprint!(
                "{}",
                render_sampled_channel_exchange_report(
                    &channel,
                    &sampled_channel_exchange_report,
                    sampled_channel_exchange_bounds.wanted_image_count
                )
            );
            // A short sample fails: a harness that found fewer frames than it asked for must not
            // read exit 0 as "this is all the channel had".
            let every_wanted_frame_was_written =
                sampled_channel_exchange_report.written_image_paths.len()
                    == sampled_channel_exchange_bounds.wanted_image_count;
            Ok(if every_wanted_frame_was_written { 0 } else { 1 })
        }
    }
}

/// The form `exchange_arguments` asks for, or the usage refusal. An empty SURFACE_ID or
/// `--channel` counts as absent, and an empty `--field` as the default.
pub(crate) fn surface_image_exchange_form(
    exchange_arguments: &SurfaceImageExchangeArguments,
) -> Result<SurfaceImageExchangeForm, TatolabCommandFailure> {
    let published_surface_id = exchange_arguments
        .published_surface_id
        .as_deref()
        .filter(|published_surface_id| !published_surface_id.is_empty());
    let channel = exchange_arguments
        .channel
        .as_deref()
        .filter(|channel| !channel.is_empty());
    match (published_surface_id, channel) {
        (Some(_), Some(_)) => Err(TatolabCommandFailure::refused(
            "`tatolab exchange` takes a surface id or `--channel`, not both. One id is one \
             exchange; `--channel` samples ids off a channel."
                .to_owned(),
        )),
        (None, None) => Err(TatolabCommandFailure::refused(
            "`tatolab exchange` needs a surface id or `--channel`. Ids come from bags — \
             `tatolab tap <channel>` shows what one carries."
                .to_owned(),
        )),
        (Some(published_surface_id), None) => {
            let channel_form_flags_given: Vec<&str> = [
                (
                    "--count",
                    exchange_arguments.requested_frame_count.is_some(),
                ),
                (
                    "--every",
                    exchange_arguments.requested_every_nth_bag.is_some(),
                ),
                (
                    "--field",
                    exchange_arguments
                        .requested_surface_id_bag_field_name
                        .is_some(),
                ),
            ]
            .into_iter()
            .filter_map(|(channel_form_flag, given)| given.then_some(channel_form_flag))
            .collect();
            if !channel_form_flags_given.is_empty() {
                return Err(TatolabCommandFailure::refused(format!(
                    "{} sample a channel, and a surface id names one frame already. Use \
                     `--channel` instead of SURFACE_ID.",
                    channel_form_flags_given.join(", ")
                )));
            }
            Ok(SurfaceImageExchangeForm::OnePublishedSurfaceId {
                published_surface_id: published_surface_id.to_owned(),
            })
        }
        (None, Some(channel)) => {
            let wanted_image_count =
                sample_bound_at_least_one("--count", exchange_arguments.requested_frame_count)?;
            let every_nth_bag =
                sample_bound_at_least_one("--every", exchange_arguments.requested_every_nth_bag)?;
            let surface_id_bag_field_name = exchange_arguments
                .requested_surface_id_bag_field_name
                .as_deref()
                .filter(|surface_id_bag_field_name| !surface_id_bag_field_name.is_empty())
                .unwrap_or(DEFAULT_SURFACE_ID_BAG_FIELD_NAME);
            Ok(SurfaceImageExchangeForm::SampledChannel {
                channel: channel.to_owned(),
                sampled_channel_exchange_bounds: SampledChannelExchangeBounds {
                    wanted_image_count,
                    every_nth_bag,
                    surface_id_bag_field_name: surface_id_bag_field_name.to_owned(),
                },
            })
        }
    }
}

/// `requested_sample_bound`, 1 when absent, refused below 1 naming `sample_bound_flag`.
fn sample_bound_at_least_one(
    sample_bound_flag: &str,
    requested_sample_bound: Option<i64>,
) -> Result<usize, TatolabCommandFailure> {
    let requested_sample_bound = requested_sample_bound.unwrap_or(1);
    if requested_sample_bound < 1 {
        return Err(TatolabCommandFailure::refused(format!(
            "`{sample_bound_flag}` must be at least 1."
        )));
    }
    Ok(usize::try_from(requested_sample_bound).unwrap_or(usize::MAX))
}

/// `output_directory` with its `.` components dropped, so a written path reads `<dir>/<file>`
/// however `--out` spelled the directory; `.` when nothing else is left.
fn output_directory_without_current_directory_components(output_directory: &Path) -> PathBuf {
    let output_directory_components: PathBuf = output_directory
        .components()
        .filter(|output_directory_component| {
            !matches!(output_directory_component, Component::CurDir)
        })
        .collect();
    if output_directory_components.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        output_directory_components
    }
}

/// Exchange `published_surface_id` for its frame's exact pixels and write them into
/// `output_directory` as `<sanitized id>.png`, answering the written path.
pub(crate) fn exchange_one_published_surface_id_into_directory(
    local_api_connection: &mut LocalApiConnection,
    published_surface_id: &str,
    output_directory: &Path,
) -> Result<PathBuf, SurfaceImageExchangeFailure> {
    let exchanged_surface_image =
        fetch_surface_image_png_bytes(local_api_connection, published_surface_id)?;
    write_exchanged_surface_image(
        output_directory,
        &format!(
            "{}.png",
            file_name_stem_for_surface_id(published_surface_id)
        ),
        &exchanged_surface_image,
    )
}

/// Exchange one published surface id for its frame's exact PNG bytes over the local API's REST
/// route. Any status outside `2xx` is a refusal carrying the status and the reason given.
pub(crate) fn fetch_surface_image_png_bytes(
    local_api_connection: &mut LocalApiConnection,
    published_surface_id: &str,
) -> Result<ExchangedSurfaceImage, SurfaceImageExchangeFailure> {
    let answered = local_api_connection.get_whole_response(
        &surface_image_exchange_route_path_for_surface_id(published_surface_id),
    )?;
    if !answered.status.is_success() {
        return Err(SurfaceImageExchangeFailure::RefusedByTheRuntime {
            published_surface_id: published_surface_id.to_owned(),
            http_status: answered.status,
            refusal_detail: refusal_detail_of_the_exchange_route(&answered.body),
        });
    }
    Ok(ExchangedSurfaceImage {
        png_image_bytes: answered.body,
        source_surface_pixel_width: source_surface_pixel_extent(
            &answered.headers,
            SURFACE_PIXEL_WIDTH_HEADER_NAME,
        ),
        source_surface_pixel_height: source_surface_pixel_extent(
            &answered.headers,
            SURFACE_PIXEL_HEIGHT_HEADER_NAME,
        ),
    })
}

/// The message out of the route's `{"error": …}` refusal body, or the body's trimmed text when it
/// carries none.
fn refusal_detail_of_the_exchange_route(refusal_body: &[u8]) -> String {
    let refusal_text = String::from_utf8_lossy(refusal_body).trim().to_owned();
    match serde_json::from_str::<serde_json::Value>(&refusal_text) {
        Ok(serde_json::Value::Object(refusal_fields)) => match refusal_fields.get("error") {
            Some(serde_json::Value::String(refusal_message)) => refusal_message.clone(),
            _ => refusal_text,
        },
        _ => refusal_text,
    }
}

/// One extent header as a pixel count; absent or malformed is `None`, since the extent only
/// annotates pixels that did come back.
fn source_surface_pixel_extent(
    response_headers: &HeaderMap,
    extent_header_name: &str,
) -> Option<u32> {
    response_headers
        .get(extent_header_name)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// `published_surface_id` as a file-name stem: every character outside `[A-Za-z0-9._-]` folded to
/// `_`.
fn file_name_stem_for_surface_id(published_surface_id: &str) -> String {
    let file_name_stem: String = published_surface_id
        .chars()
        .map(|surface_id_character| {
            if surface_id_character.is_ascii_alphanumeric()
                || matches!(surface_id_character, '.' | '_' | '-')
            {
                surface_id_character
            } else {
                '_'
            }
        })
        .collect();
    if file_name_stem.is_empty() {
        FILE_NAME_STEM_OF_AN_EMPTY_SURFACE_ID.to_owned()
    } else {
        file_name_stem
    }
}

/// Write `exchanged_surface_image` into `output_directory`, creating it when absent, as
/// `file_name`.
fn write_exchanged_surface_image(
    output_directory: &Path,
    file_name: &str,
    exchanged_surface_image: &ExchangedSurfaceImage,
) -> Result<PathBuf, SurfaceImageExchangeFailure> {
    let output_directory_not_writable =
        |write_failure| SurfaceImageExchangeFailure::OutputDirectoryNotWritable {
            output_directory: output_directory.to_path_buf(),
            write_failure,
        };
    std::fs::create_dir_all(output_directory).map_err(output_directory_not_writable)?;
    let written_image_path = output_directory.join(file_name);
    std::fs::write(
        &written_image_path,
        &exchanged_surface_image.png_image_bytes,
    )
    .map_err(output_directory_not_writable)?;
    Ok(written_image_path)
}

/// Tap `channel` through `local_api_connection`, exchange the surface ids its sampled bags carry
/// over the same connection, and write the PNGs into `output_directory` as
/// `<0000>-<sanitized id>.png`.
///
/// The stride counts the bags this client received, continuing across tap rounds rather than
/// restarting per round. Each round is a fresh attach, so it is not a stride over the channel.
pub(crate) fn sample_channel_into_exchanged_surface_images(
    local_api_connection: &mut LocalApiConnection,
    channel: &str,
    output_directory: &Path,
    sampled_channel_exchange_bounds: &SampledChannelExchangeBounds,
) -> SampledChannelExchangeReport {
    let mut sampled_channel_exchange_report = SampledChannelExchangeReport::default();
    if let Err(sampled_channel_exchange_stop) = exchange_sampled_bags_across_tap_rounds(
        local_api_connection,
        channel,
        output_directory,
        sampled_channel_exchange_bounds,
        &mut sampled_channel_exchange_report,
    ) {
        sampled_channel_exchange_report.stopped_early_because = Some(sampled_channel_exchange_stop);
    }
    sampled_channel_exchange_report
}

/// The channel form's tap rounds, accumulating into `sampled_channel_exchange_report`; the error
/// is why the run stopped early, and the report keeps everything gathered before it.
fn exchange_sampled_bags_across_tap_rounds(
    local_api_connection: &mut LocalApiConnection,
    channel: &str,
    output_directory: &Path,
    sampled_channel_exchange_bounds: &SampledChannelExchangeBounds,
    sampled_channel_exchange_report: &mut SampledChannelExchangeReport,
) -> Result<(), SampledChannelExchangeStop> {
    let SampledChannelExchangeBounds {
        wanted_image_count,
        every_nth_bag,
        surface_id_bag_field_name,
    } = sampled_channel_exchange_bounds;
    while sampled_channel_exchange_report.written_image_paths.len() < *wanted_image_count
        && sampled_channel_exchange_report.tap_rounds < MAX_TAP_ROUNDS_PER_SAMPLE_RUN
    {
        sampled_channel_exchange_report.tap_rounds += 1;
        let still_wanted_image_count =
            wanted_image_count - sampled_channel_exchange_report.written_image_paths.len();
        let tap_tool_result_text = call_tap_on_the_local_api_connection(
            local_api_connection,
            channel,
            still_wanted_image_count.saturating_mul(*every_nth_bag),
        )?;
        for tapped_bag in tapped_channel_bag_frames(&tap_tool_result_text, channel)? {
            let selected_by_the_stride = sampled_channel_exchange_report
                .bags_examined
                .is_multiple_of(*every_nth_bag);
            sampled_channel_exchange_report.bags_examined += 1;
            if !selected_by_the_stride {
                continue;
            }
            if tapped_bag.preview_was_capped {
                return Err(
                    SampledChannelExchangeStop::SelectedBagPastTheTapPreviewCap {
                        channel: channel.to_owned(),
                        whole_bag_byte_len: tapped_bag.whole_bag_byte_len,
                    },
                );
            }
            let Some(published_surface_id) = surface_id_in_tapped_bag(
                &tapped_bag.framed_bag_bytes,
                channel,
                surface_id_bag_field_name,
            )?
            else {
                sampled_channel_exchange_report.bags_missing_the_surface_id_field += 1;
                continue;
            };
            let exchanged_surface_image =
                match fetch_surface_image_png_bytes(local_api_connection, &published_surface_id) {
                    Ok(exchanged_surface_image) => exchanged_surface_image,
                    Err(exchange_failure) if exchange_failure.names_a_recycled_frame() => {
                        sampled_channel_exchange_report
                            .retried_recycled_surface_ids
                            .push(published_surface_id);
                        continue;
                    }
                    Err(exchange_failure) => return Err(exchange_failure.into()),
                };
            let written_image_path = write_exchanged_surface_image(
                output_directory,
                &format!(
                    "{:04}-{}.png",
                    sampled_channel_exchange_report.written_image_paths.len(),
                    file_name_stem_for_surface_id(&published_surface_id)
                ),
                &exchanged_surface_image,
            )?;
            sampled_channel_exchange_report
                .written_image_paths
                .push(written_image_path);
            if sampled_channel_exchange_report.written_image_paths.len() == *wanted_image_count {
                break;
            }
        }
    }
    Ok(())
}

/// Call `tap` for `requested_bag_count` bags on `channel` through the MCP client
/// `local_api_connection` keeps across the run's rounds.
fn call_tap_on_the_local_api_connection(
    local_api_connection: &mut LocalApiConnection,
    channel: &str,
    requested_bag_count: usize,
) -> Result<String, LocalApiMcpToolClientFailure> {
    local_api_connection
        .call_tool(
            TAP_TOOL_NAME,
            tap_tool_arguments(
                channel,
                Some(i64::try_from(requested_bag_count).unwrap_or(i64::MAX)),
                None,
            ),
        )
        .map_err(|tap_failure| {
            tool_call_failure_worded_as_an_observation_verb_reports_it(TAP_TOOL_NAME, tap_failure)
        })
}

/// The bags one `tap` result carries, as the framed bytes the channel carried. The tool
/// hex-encodes a bounded prefix of each bag and flags the ones it capped.
fn tapped_channel_bag_frames(
    tap_tool_result_text: &str,
    channel: &str,
) -> Result<Vec<TappedChannelBagFrame>, SampledChannelExchangeStop> {
    let tap_tool_result: TapToolResult =
        serde_json::from_str(tap_tool_result_text).map_err(|shape_failure| {
            SampledChannelExchangeStop::TapResultIsNotTheTapToolsShape {
                channel: channel.to_owned(),
                tap_tool_result_text: tap_tool_result_text.to_owned(),
                shape_failure,
            }
        })?;
    tap_tool_result
        .bags
        .into_iter()
        .map(|tapped_bag| {
            let framed_bag_bytes = hex::decode(&tapped_bag.hex_preview).map_err(|hex_failure| {
                SampledChannelExchangeStop::TappedBagHexPreviewDoesNotDecode {
                    channel: channel.to_owned(),
                    hex_failure,
                }
            })?;
            Ok(TappedChannelBagFrame {
                framed_bag_bytes,
                preview_was_capped: tapped_bag.hex_truncated,
                whole_bag_byte_len: tapped_bag.byte_len,
            })
        })
        .collect()
}

/// The string `surface_id_bag_field_name` holds in one tapped bag, or `None` when the bag is no
/// map, lacks the field, or holds something other than a string there. A bag that does not decode
/// at all is an error rather than a bag without the field: counting it would say the channel
/// publishes no ids when this client simply could not read it.
fn surface_id_in_tapped_bag(
    framed_bag_bytes: &[u8],
    channel: &str,
    surface_id_bag_field_name: &str,
) -> Result<Option<String>, SampledChannelExchangeStop> {
    let tapped_bag =
        decode_tapped_channel_bag_frame(framed_bag_bytes).map_err(|decode_failure| {
            SampledChannelExchangeStop::SelectedBagDoesNotDecode {
                channel: channel.to_owned(),
                decode_failure,
            }
        })?;
    let rmpv::Value::Map(tapped_bag_entries) = tapped_bag else {
        return Ok(None);
    };
    // A repeated key reads as its last entry, as a decoded map keeps it.
    Ok(tapped_bag_entries
        .iter()
        .rev()
        .find(|(entry_name, _)| entry_name.as_str() == Some(surface_id_bag_field_name))
        .and_then(|(_, entry_value)| entry_value.as_str())
        .map(str::to_owned))
}

/// One tapped bag's msgpack value, its frame header stripped by the transport's own accessor.
fn decode_tapped_channel_bag_frame(
    framed_bag_bytes: &[u8],
) -> Result<rmpv::Value, TappedChannelBagDecodeFailure> {
    let mut bag_payload = FrameHeader::payload_bounded_by_its_header(framed_bag_bytes)
        .map_err(TappedFramePayloadRefusal::from)?;
    Ok(rmpv::decode::read_value(&mut bag_payload)?)
}

/// What a channel-form run says on stderr beside the paths on stdout: what it exchanged, what it
/// retried, how many selected bags lacked the field, and why it stopped early.
pub(crate) fn render_sampled_channel_exchange_report(
    channel: &str,
    sampled_channel_exchange_report: &SampledChannelExchangeReport,
    wanted_image_count: usize,
) -> String {
    let SampledChannelExchangeReport {
        written_image_paths,
        retried_recycled_surface_ids,
        bags_missing_the_surface_id_field,
        bags_examined,
        tap_rounds,
        stopped_early_because,
    } = sampled_channel_exchange_report;
    let mut rendered_report = format!(
        "exchanged {} of {wanted_image_count} requested frames from `{channel}` ({bags_examined} \
         bags examined over {tap_rounds} tap {})\n",
        written_image_paths.len(),
        noun_agreeing_with_count(*tap_rounds, "round", "rounds")
    );
    if !retried_recycled_surface_ids.is_empty() {
        let _ = writeln!(
            rendered_report,
            "retried {} recycled {} against newer bags: {}",
            retried_recycled_surface_ids.len(),
            noun_agreeing_with_count(retried_recycled_surface_ids.len(), "frame", "frames"),
            retried_recycled_surface_ids.join(", ")
        );
    }
    if *bags_missing_the_surface_id_field > 0 {
        let _ = writeln!(
            rendered_report,
            "{bags_missing_the_surface_id_field} {} carried no surface id in the named field — \
             name the right one with `--field`",
            noun_agreeing_with_count(*bags_missing_the_surface_id_field, "bag", "bags")
        );
    }
    if let Some(sampled_channel_exchange_stop) = stopped_early_because {
        let _ = writeln!(rendered_report, "error: {sampled_channel_exchange_stop}");
    }
    rendered_report
}

/// `singular_noun` for a count of one, `plural_noun` for any other.
fn noun_agreeing_with_count<'noun, Count: PartialEq + From<u8>>(
    count: Count,
    singular_noun: &'noun str,
    plural_noun: &'noun str,
) -> &'noun str {
    if count == Count::from(1) {
        singular_noun
    } else {
        plural_noun
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use streamlib_ipc_types::FRAME_HEADER_SIZE;

    use super::*;
    use crate::isolated_node_registry::{
        IsolatedNodeRegistry, NOTHING_LISTENS_LOCAL_API_SOCKET_PATH, a_registry_entry_named,
    };
    use crate::local_api_runtime_selection::select_live_runtime_in_node_registry;
    use crate::stub_local_api_server::{
        StubLocalApiScript, StubLocalApiServer, StubSurfaceImageAnswer, StubToolAnswer,
        surface_image_answers_by_id,
    };
    use crate::tapped_channel_bag_fixtures::{
        CAPPED_BAG_STATED_BYTE_LEN, FIXTURE_CHANNEL, SLICE_HOLDS_ONLY_THE_BAG,
        bag_publishing_no_surface_id, bag_publishing_surface_id,
        bag_publishing_surface_id_in_field, framed_bag, labelled_png_image_answer,
        msgpack_named_map, png_bytes_for, png_files_in, tap_result_text,
        tap_result_text_capping_bags,
    };

    const RECYCLED_FRAME_ERROR_MESSAGE: &str =
        "surface frame recycled: slot reused since that generation";

    fn recycled_frame_answer() -> StubSurfaceImageAnswer {
        StubSurfaceImageAnswer::refusal(410, RECYCLED_FRAME_ERROR_MESSAGE)
    }

    fn local_api_connection_to(local_api_socket_path: &Path) -> LocalApiConnection {
        LocalApiConnection::open(local_api_socket_path, OBSERVATION_VERB_TOOL_CALL_TIMEOUT).unwrap()
    }

    fn sampling_bounds(
        wanted_image_count: usize,
        every_nth_bag: usize,
    ) -> SampledChannelExchangeBounds {
        SampledChannelExchangeBounds {
            wanted_image_count,
            every_nth_bag,
            surface_id_bag_field_name: DEFAULT_SURFACE_ID_BAG_FIELD_NAME.to_owned(),
        }
    }

    fn sample_the_stub_channel(
        local_api_socket_path: &Path,
        output_directory: &Path,
        sampled_channel_exchange_bounds: &SampledChannelExchangeBounds,
    ) -> SampledChannelExchangeReport {
        sample_channel_into_exchanged_surface_images(
            &mut local_api_connection_to(local_api_socket_path),
            FIXTURE_CHANNEL,
            output_directory,
            sampled_channel_exchange_bounds,
        )
    }

    /// Why `sampled_channel_exchange_report`'s run stopped early, as the report prints it.
    fn stop_reason_as_printed(
        sampled_channel_exchange_report: &SampledChannelExchangeReport,
    ) -> Option<String> {
        sampled_channel_exchange_report
            .stopped_early_because
            .as_ref()
            .map(ToString::to_string)
    }

    /// [`surface_id_in_tapped_bag`], its refusal as the report prints it.
    fn surface_id_in_tapped_bag_as_printed(
        framed_bag_bytes: &[u8],
        channel: &str,
        surface_id_bag_field_name: &str,
    ) -> Result<Option<String>, String> {
        surface_id_in_tapped_bag(framed_bag_bytes, channel, surface_id_bag_field_name)
            .map_err(|sampled_channel_exchange_stop| sampled_channel_exchange_stop.to_string())
    }

    fn written_image_contents(
        sampled_channel_exchange_report: &SampledChannelExchangeReport,
    ) -> Vec<Vec<u8>> {
        sampled_channel_exchange_report
            .written_image_paths
            .iter()
            .map(|written_image_path| std::fs::read(written_image_path).unwrap())
            .collect()
    }

    fn exchange_arguments(
        published_surface_id: Option<&str>,
        channel: Option<&str>,
    ) -> SurfaceImageExchangeArguments {
        SurfaceImageExchangeArguments {
            published_surface_id: published_surface_id.map(str::to_owned),
            output_directory: OsString::from("frames"),
            channel: channel.map(str::to_owned),
            requested_frame_count: None,
            requested_every_nth_bag: None,
            requested_surface_id_bag_field_name: None,
            runtime_target: RuntimeTargetArguments::default(),
        }
    }

    fn usage_refusal(exchange_arguments: &SurfaceImageExchangeArguments) -> String {
        TatolabCommandFailure::refusal_message_of(surface_image_exchange_form(exchange_arguments))
    }

    // The REST spelling of the exchange.

    /// A bare `#` would make the generation a URL fragment the runtime never sees.
    #[test]
    fn a_pooled_frame_id_is_percent_encoded_into_the_route() {
        let stub_local_api_server = StubLocalApiServer::serve_answering_surface_images([(
            "cam/frame#7",
            labelled_png_image_answer("seven"),
        )]);

        let exchanged = fetch_surface_image_png_bytes(
            &mut local_api_connection_to(&stub_local_api_server.local_api_socket_path),
            "cam/frame#7",
        )
        .unwrap();

        assert_eq!(exchanged.png_image_bytes.as_ref(), png_bytes_for("seven"));
        assert_eq!(
            stub_local_api_server.recorded_image_request_paths(),
            ["/api/surfaces/cam%2Fframe%237/image"]
        );
    }

    #[test]
    fn the_exchange_states_the_surfaces_own_extent() {
        let stub_local_api_server = StubLocalApiServer::serve_answering_surface_images([(
            "s#1",
            labelled_png_image_answer("one"),
        )]);

        let exchanged = fetch_surface_image_png_bytes(
            &mut local_api_connection_to(&stub_local_api_server.local_api_socket_path),
            "s#1",
        )
        .unwrap();

        assert_eq!(exchanged.source_surface_pixel_width, Some(1920));
        assert_eq!(exchanged.source_surface_pixel_height, Some(1080));
    }

    #[test]
    fn an_extent_header_absent_or_malformed_is_no_extent() {
        let stub_local_api_server = StubLocalApiServer::serve_answering_surface_images([(
            "s#1",
            StubSurfaceImageAnswer::png_image(b"png", None, Some(1080)),
        )]);

        let exchanged = fetch_surface_image_png_bytes(
            &mut local_api_connection_to(&stub_local_api_server.local_api_socket_path),
            "s#1",
        )
        .unwrap();

        assert_eq!(exchanged.source_surface_pixel_width, None);
        assert_eq!(exchanged.source_surface_pixel_height, Some(1080));
        let mut malformed_extent_headers = HeaderMap::new();
        malformed_extent_headers.insert(SURFACE_PIXEL_WIDTH_HEADER_NAME, "wide".parse().unwrap());
        assert_eq!(
            source_surface_pixel_extent(&malformed_extent_headers, SURFACE_PIXEL_WIDTH_HEADER_NAME),
            None
        );
    }

    #[test]
    fn a_recycled_frame_is_a_refusal_that_composes_as_a_retry() {
        let stub_local_api_server =
            StubLocalApiServer::serve_answering_surface_images([("s#1", recycled_frame_answer())]);

        let refused = fetch_surface_image_png_bytes(
            &mut local_api_connection_to(&stub_local_api_server.local_api_socket_path),
            "s#1",
        )
        .unwrap_err();

        assert!(refused.names_a_recycled_frame());
        assert_eq!(
            refused.to_string(),
            format!("exchange of surface `s#1` answered 410: {RECYCLED_FRAME_ERROR_MESSAGE}")
        );
    }

    /// A surface that never existed, or a format with no conversion arm, refuses identically
    /// forever, so retrying it would spin rather than recover.
    #[test]
    fn a_refusal_that_is_not_a_recycled_frame_does_not_compose() {
        for refused_status in [404, 501] {
            let stub_local_api_server = StubLocalApiServer::serve_answering_surface_images([(
                "s#1",
                StubSurfaceImageAnswer::refusal(refused_status, "no"),
            )]);

            let refused = fetch_surface_image_png_bytes(
                &mut local_api_connection_to(&stub_local_api_server.local_api_socket_path),
                "s#1",
            )
            .unwrap_err();

            assert!(!refused.names_a_recycled_frame(), "{refused_status}");
            assert!(
                matches!(
                    refused,
                    SurfaceImageExchangeFailure::RefusedByTheRuntime { http_status, .. }
                        if http_status.as_u16() == refused_status
                ),
                "{refused:?}"
            );
        }
    }

    #[test]
    fn a_refusal_body_yields_its_error_message_or_else_its_trimmed_text() {
        for (refusal_body, refusal_detail) in [
            (r#"{"error": "no such surface"}"#, "no such surface"),
            ("  frame gone\n", "frame gone"),
            (r#"{"error": 5}"#, r#"{"error": 5}"#),
            ("[1, 2]", "[1, 2]"),
            ("", ""),
        ] {
            assert_eq!(
                refusal_detail_of_the_exchange_route(refusal_body.as_bytes()),
                refusal_detail,
                "{refusal_body:?}"
            );
        }
        assert_eq!(
            SurfaceImageExchangeFailure::RefusedByTheRuntime {
                published_surface_id: "s#1".to_owned(),
                http_status: StatusCode::NOT_FOUND,
                refusal_detail: String::new(),
            }
            .to_string(),
            "exchange of surface `s#1` answered 404"
        );
    }

    #[test]
    fn an_exchange_nothing_answers_is_named_as_unreachable() {
        let unreachable = fetch_surface_image_png_bytes(
            &mut local_api_connection_to(Path::new(NOTHING_LISTENS_LOCAL_API_SOCKET_PATH)),
            "s#1",
        )
        .unwrap_err();

        assert!(!unreachable.names_a_recycled_frame());
        assert!(
            unreachable.to_string().starts_with(&format!(
                "no control plane reachable at {NOTHING_LISTENS_LOCAL_API_SOCKET_PATH} ("
            )),
            "{unreachable}"
        );
    }

    // The id form.

    #[test]
    fn the_id_form_writes_the_exact_bytes_into_a_directory_it_creates() {
        let stub_local_api_server = StubLocalApiServer::serve_answering_surface_images([(
            "cam/frame#7",
            labelled_png_image_answer("seven"),
        )]);
        let scratch_directory = tempfile::tempdir().unwrap();
        let output_directory = scratch_directory.path().join("nested").join("frames");

        let written_image_path = exchange_one_published_surface_id_into_directory(
            &mut local_api_connection_to(&stub_local_api_server.local_api_socket_path),
            "cam/frame#7",
            &output_directory,
        )
        .unwrap();

        assert_eq!(written_image_path, output_directory.join("cam_frame_7.png"));
        assert_eq!(
            std::fs::read(&written_image_path).unwrap(),
            png_bytes_for("seven")
        );
        assert_eq!(png_files_in(&output_directory), 1);
    }

    #[test]
    fn the_id_form_reaches_a_registered_runtime_named_by_the_node_flag() {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let stub_local_api_server = StubLocalApiServer::serve_answering_surface_images([(
            "cam/frame#7",
            labelled_png_image_answer("seven"),
        )]);
        let other_stub_local_api_server = StubLocalApiServer::serve_default();
        isolated_node_registry.write_registry_entry(&a_registry_entry_named(
            "Rcam",
            "rig-cam",
            &stub_local_api_server.local_api_socket_path,
        ));
        isolated_node_registry.write_registry_entry(&a_registry_entry_named(
            "Rother",
            "rig-other",
            &other_stub_local_api_server.local_api_socket_path,
        ));
        let output_directory = tempfile::tempdir().unwrap();

        let selected_runtime = select_live_runtime_in_node_registry(
            &isolated_node_registry.node_registry_directory(),
            Some("rig-cam"),
        )
        .unwrap();
        let written_image_path = exchange_one_published_surface_id_into_directory(
            &mut local_api_connection_to(&selected_runtime.local_api_socket_path),
            "cam/frame#7",
            output_directory.path(),
        )
        .unwrap();

        assert_eq!(
            std::fs::read(written_image_path).unwrap(),
            png_bytes_for("seven")
        );
        assert_eq!(
            stub_local_api_server.recorded_image_request_paths(),
            ["/api/surfaces/cam%2Fframe%237/image"]
        );
        assert!(
            other_stub_local_api_server
                .recorded_image_request_paths()
                .is_empty()
        );
    }

    #[test]
    fn a_surface_id_that_does_not_resolve_writes_nothing_and_names_the_id() {
        let stub_local_api_server = StubLocalApiServer::serve_answering_surface_images(Vec::<(
            String,
            StubSurfaceImageAnswer,
        )>::new(
        ));
        let output_directory = tempfile::tempdir().unwrap();

        let refused = exchange_one_published_surface_id_into_directory(
            &mut local_api_connection_to(&stub_local_api_server.local_api_socket_path),
            "gone#1",
            output_directory.path(),
        )
        .unwrap_err();

        assert_eq!(
            refused.to_string(),
            "exchange of surface `gone#1` answered 404: no such surface"
        );
        assert_eq!(png_files_in(output_directory.path()), 0);
    }

    /// `--out` naming an existing regular file is a typo, and a typo gets a message.
    #[test]
    fn an_output_directory_that_cannot_be_written_is_reported() {
        let stub_local_api_server = StubLocalApiServer::serve_answering_surface_images([(
            "s#1",
            labelled_png_image_answer("one"),
        )]);
        let scratch_directory = tempfile::tempdir().unwrap();
        let already_a_file = scratch_directory.path().join("already-a-file");
        std::fs::write(&already_a_file, "not a directory").unwrap();

        let refused = exchange_one_published_surface_id_into_directory(
            &mut local_api_connection_to(&stub_local_api_server.local_api_socket_path),
            "s#1",
            &already_a_file,
        )
        .unwrap_err();

        assert!(
            refused.to_string().starts_with(&format!(
                "could not write into `{}`: ",
                already_a_file.display()
            )),
            "{refused}"
        );
    }

    #[test]
    fn a_surface_id_folds_into_a_file_name_stem() {
        for (published_surface_id, file_name_stem) in [
            ("cam/frame#7", "cam_frame_7"),
            ("a.b-c_D9", "a.b-c_D9"),
            ("é#1", "__1"),
            ("", "surface"),
        ] {
            assert_eq!(
                file_name_stem_for_surface_id(published_surface_id),
                file_name_stem,
                "{published_surface_id:?}"
            );
        }
    }

    #[test]
    fn the_output_directory_drops_its_current_directory_components() {
        for (spelled_output_directory, output_directory) in [
            ("frames", "frames"),
            ("./frames/", "frames"),
            ("a/./b//c", "a/b/c"),
            ("/tmp/frames", "/tmp/frames"),
            (".", "."),
            ("", "."),
            ("../frames", "../frames"),
        ] {
            assert_eq!(
                output_directory_without_current_directory_components(Path::new(
                    spelled_output_directory
                )),
                PathBuf::from(output_directory),
                "{spelled_output_directory:?}"
            );
        }
    }

    // Usage.

    #[test]
    fn an_id_or_a_channel_picks_the_form_with_the_channel_forms_defaults() {
        assert_eq!(
            surface_image_exchange_form(&exchange_arguments(Some("s#1"), None)).unwrap(),
            SurfaceImageExchangeForm::OnePublishedSurfaceId {
                published_surface_id: "s#1".to_owned()
            }
        );
        assert_eq!(
            surface_image_exchange_form(&exchange_arguments(None, Some("cam/frame"))).unwrap(),
            SurfaceImageExchangeForm::SampledChannel {
                channel: "cam/frame".to_owned(),
                sampled_channel_exchange_bounds: sampling_bounds(1, 1),
            }
        );
        let mut every_bound_named = exchange_arguments(None, Some("cam/frame"));
        every_bound_named.requested_frame_count = Some(3);
        every_bound_named.requested_every_nth_bag = Some(2);
        every_bound_named.requested_surface_id_bag_field_name = Some("frame_id".to_owned());
        assert_eq!(
            surface_image_exchange_form(&every_bound_named).unwrap(),
            SurfaceImageExchangeForm::SampledChannel {
                channel: "cam/frame".to_owned(),
                sampled_channel_exchange_bounds: SampledChannelExchangeBounds {
                    wanted_image_count: 3,
                    every_nth_bag: 2,
                    surface_id_bag_field_name: "frame_id".to_owned(),
                },
            }
        );
    }

    #[test]
    fn exchange_refuses_a_surface_id_and_a_channel_together() {
        assert_eq!(
            usage_refusal(&exchange_arguments(Some("s#1"), Some("cam/frame"))),
            "`tatolab exchange` takes a surface id or `--channel`, not both. One id is one \
             exchange; `--channel` samples ids off a channel."
        );
    }

    #[test]
    fn exchange_needs_a_surface_id_or_a_channel() {
        for (published_surface_id, channel) in [(None, None), (Some(""), Some(""))] {
            assert_eq!(
                usage_refusal(&exchange_arguments(published_surface_id, channel)),
                "`tatolab exchange` needs a surface id or `--channel`. Ids come from bags — \
                 `tatolab tap <channel>` shows what one carries."
            );
        }
    }

    /// An empty id or channel counts as none given, and an empty `--field` as the default.
    #[test]
    fn an_empty_id_channel_or_field_counts_as_not_given() {
        assert!(matches!(
            surface_image_exchange_form(&exchange_arguments(Some("s#1"), Some(""))),
            Ok(SurfaceImageExchangeForm::OnePublishedSurfaceId { .. })
        ));
        let mut empty_field = exchange_arguments(Some(""), Some("cam/frame"));
        empty_field.requested_surface_id_bag_field_name = Some(String::new());
        assert_eq!(
            surface_image_exchange_form(&empty_field).unwrap(),
            SurfaceImageExchangeForm::SampledChannel {
                channel: "cam/frame".to_owned(),
                sampled_channel_exchange_bounds: sampling_bounds(1, 1),
            }
        );
    }

    /// These sample a channel; a surface id already names one frame. Asking explicitly for the
    /// value the channel form defaults to is still asking for the channel form.
    #[test]
    fn a_channel_form_flag_beside_a_surface_id_is_refused_by_name() {
        let mut with_count = exchange_arguments(Some("s#1"), None);
        with_count.requested_frame_count = Some(1);
        let mut with_every = exchange_arguments(Some("s#1"), None);
        with_every.requested_every_nth_bag = Some(2);
        let mut with_field = exchange_arguments(Some("s#1"), None);
        with_field.requested_surface_id_bag_field_name = Some(String::new());
        let mut with_all_three = exchange_arguments(Some("s#1"), None);
        with_all_three.requested_frame_count = Some(3);
        with_all_three.requested_every_nth_bag = Some(2);
        with_all_three.requested_surface_id_bag_field_name = Some("frame_id".to_owned());

        for (channel_form_arguments, named_flags) in [
            (with_count, "--count"),
            (with_every, "--every"),
            (with_field, "--field"),
            (with_all_three, "--count, --every, --field"),
        ] {
            assert_eq!(
                usage_refusal(&channel_form_arguments),
                format!(
                    "{named_flags} sample a channel, and a surface id names one frame already. \
                     Use `--channel` instead of SURFACE_ID."
                )
            );
        }
    }

    #[test]
    fn a_sample_bound_below_one_is_refused_by_name() {
        for (sample_bound_flag, below_one) in [("--count", 0), ("--count", -1), ("--every", 0)] {
            let mut below_one_bound = exchange_arguments(None, Some("cam/frame"));
            if sample_bound_flag == "--count" {
                below_one_bound.requested_frame_count = Some(below_one);
            } else {
                below_one_bound.requested_every_nth_bag = Some(below_one);
            }

            assert_eq!(
                usage_refusal(&below_one_bound),
                format!("`{sample_bound_flag}` must be at least 1.")
            );
        }
    }

    // Reading a tapped bag.

    #[test]
    fn a_tapped_bag_decodes_past_the_slack_its_slice_carries() {
        assert_eq!(
            surface_id_in_tapped_bag_as_printed(
                &bag_publishing_surface_id("camera/frame#7"),
                FIXTURE_CHANNEL,
                "surface_id"
            ),
            Ok(Some("camera/frame#7".to_owned()))
        );
    }

    #[test]
    fn bytes_too_short_to_hold_a_frame_header_are_refused() {
        assert_eq!(
            surface_id_in_tapped_bag_as_printed(&[0u8; 8], FIXTURE_CHANNEL, "surface_id"),
            Err(
                "a bag from `cam/frame` could not be decoded: a tapped bag carries a 76-byte \
                 frame header; got 8 bytes, which cannot hold one"
                    .to_owned()
            )
        );
    }

    /// Decoding a truncated prefix would hand back a bag missing its later fields.
    #[test]
    fn a_bag_whose_header_declares_more_than_followed_is_refused_as_truncated() {
        let whole_bag = framed_bag(
            &msgpack_named_map(&[
                ("surface_id", "s#1".into()),
                ("filler", "x".repeat(200).into()),
            ]),
            SLICE_HOLDS_ONLY_THE_BAG,
        );
        let declared_payload_byte_len = whole_bag.len() - FRAME_HEADER_SIZE;

        assert_eq!(
            surface_id_in_tapped_bag_as_printed(
                &whole_bag[..whole_bag.len() - 32],
                FIXTURE_CHANNEL,
                "surface_id"
            ),
            Err(format!(
                "a bag from `cam/frame` could not be decoded: the tapped bag's header declares a \
                 {declared_payload_byte_len}-byte payload but only {} bytes followed it — the \
                 sample arrived truncated, and decoding it would invent a bag the channel never \
                 carried",
                declared_payload_byte_len - 32
            ))
        );
    }

    /// A one-entry map marker with no entry behind it.
    #[test]
    fn a_payload_that_is_not_msgpack_is_refused() {
        let refused = surface_id_in_tapped_bag_as_printed(
            &framed_bag(&[0x81], SLICE_HOLDS_ONLY_THE_BAG),
            FIXTURE_CHANNEL,
            "surface_id",
        )
        .unwrap_err();

        assert!(
            refused.starts_with("a bag from `cam/frame` could not be decoded: "),
            "{refused}"
        );
    }

    #[test]
    fn a_bag_holding_no_string_in_the_field_is_missing_it() {
        let bag_from_payload =
            |bag_payload: &[u8]| framed_bag(bag_payload, SLICE_HOLDS_ONLY_THE_BAG);
        let mut not_a_named_map = Vec::new();
        rmpv::encode::write_value(&mut not_a_named_map, &rmpv::Value::from("s#1")).unwrap();

        for bag_without_the_field in [
            bag_publishing_no_surface_id(),
            bag_publishing_surface_id_in_field("s#1", "rendered_surface"),
            bag_from_payload(&msgpack_named_map(&[("surface_id", 7.into())])),
            bag_from_payload(&msgpack_named_map(&[(
                "surface_id",
                rmpv::Value::Binary(b"s#1".to_vec()),
            )])),
            bag_from_payload(&not_a_named_map),
        ] {
            assert_eq!(
                surface_id_in_tapped_bag_as_printed(
                    &bag_without_the_field,
                    FIXTURE_CHANNEL,
                    "surface_id"
                ),
                Ok(None)
            );
        }
    }

    #[test]
    fn a_repeated_field_reads_as_its_last_entry() {
        let bag_repeating_the_field = framed_bag(
            &msgpack_named_map(&[("surface_id", "s#1".into()), ("surface_id", "s#2".into())]),
            SLICE_HOLDS_ONLY_THE_BAG,
        );

        assert_eq!(
            surface_id_in_tapped_bag_as_printed(
                &bag_repeating_the_field,
                FIXTURE_CHANNEL,
                "surface_id"
            ),
            Ok(Some("s#2".to_owned()))
        );
    }

    #[test]
    fn a_tap_result_carries_each_bags_bytes_cap_and_stated_size() {
        let first_bag = bag_publishing_surface_id("s#1");
        let second_bag = bag_publishing_surface_id("s#2");

        let tapped_bags = tapped_channel_bag_frames(
            &tap_result_text_capping_bags(&[first_bag.clone(), second_bag.clone()], &[1]),
            FIXTURE_CHANNEL,
        )
        .unwrap();

        assert_eq!(
            tapped_bags,
            [
                TappedChannelBagFrame {
                    whole_bag_byte_len: u64::try_from(first_bag.len()).unwrap(),
                    framed_bag_bytes: first_bag,
                    preview_was_capped: false,
                },
                TappedChannelBagFrame {
                    framed_bag_bytes: second_bag,
                    preview_was_capped: true,
                    whole_bag_byte_len: CAPPED_BAG_STATED_BYTE_LEN,
                },
            ]
        );
    }

    /// A one-bag tap result whose bag is `tapped_bag`, every other key as the tool writes it.
    fn tap_result_text_whose_bag_is(tapped_bag: serde_json::Value) -> String {
        let mut tap_tool_result: serde_json::Value =
            serde_json::from_str(&tap_result_text(&[bag_publishing_surface_id("s#1")])).unwrap();
        tap_tool_result["bags"][0] = tapped_bag;
        tap_tool_result.to_string()
    }

    /// A cap is a bool and a size a whole number: a result spelling either another way, or
    /// leaving a key out, is not the tap tool's, and reading a guess out of it would misdiagnose
    /// the bag.
    #[test]
    fn a_tap_result_that_is_not_the_tap_tools_shape_is_refused_naming_the_channel() {
        let not_the_tap_tools_shape = [
            "not json".to_owned(),
            r#"{"received": 0}"#.to_owned(),
            tap_result_text_whose_bag_is(json!({"byte_len": 5, "hex_truncated": false})),
            tap_result_text_whose_bag_is(json!({"hex_preview": "", "hex_truncated": false})),
            tap_result_text_whose_bag_is(json!({"byte_len": 5, "hex_preview": ""})),
            tap_result_text_whose_bag_is(
                json!({"byte_len": 5, "hex_preview": "", "hex_truncated": 1}),
            ),
            tap_result_text_whose_bag_is(
                json!({"byte_len": 5.0, "hex_preview": "", "hex_truncated": false}),
            ),
            tap_result_text_whose_bag_is(
                json!({"byte_len": "5", "hex_preview": "", "hex_truncated": false}),
            ),
        ];
        for tap_tool_result_text in not_the_tap_tools_shape {
            let refusal = tapped_channel_bag_frames(&tap_tool_result_text, FIXTURE_CHANNEL)
                .unwrap_err()
                .to_string();

            assert!(
                refusal.starts_with(
                    "tap of `cam/frame` returned a result that is not the tap tool's ("
                ),
                "{refusal}"
            );
            assert!(
                refusal.ends_with(&format!("): {tap_tool_result_text}")),
                "{refusal}"
            );
        }
    }

    /// The tool writes two lowercase digits per byte with nothing between them.
    #[test]
    fn a_hex_preview_that_is_not_unspaced_hex_is_refused_naming_the_channel() {
        for (hex_preview, hex_failure) in [
            (
                "0a ff 10",
                hex::FromHexError::InvalidHexCharacter { c: ' ', index: 2 },
            ),
            ("abc", hex::FromHexError::OddLength),
            (
                "0g",
                hex::FromHexError::InvalidHexCharacter { c: 'g', index: 1 },
            ),
        ] {
            assert_eq!(
                tapped_channel_bag_frames(
                    &tap_result_text_whose_bag_is(json!({
                        "byte_len": hex_preview.len() / 2,
                        "hex_preview": hex_preview,
                        "hex_truncated": false,
                    })),
                    FIXTURE_CHANNEL
                )
                .unwrap_err()
                .to_string(),
                format!(
                    "tap of `cam/frame` returned a bag whose hex preview does not decode: \
                     {hex_failure}"
                ),
                "{hex_preview:?}"
            );
        }
    }

    #[test]
    fn a_capped_bag_is_named_with_its_size() {
        assert_eq!(
            SampledChannelExchangeStop::SelectedBagPastTheTapPreviewCap {
                channel: "cam/frame".to_owned(),
                whole_bag_byte_len: 9000,
            }
            .to_string(),
            "a bag the sample selected on `cam/frame` is 9000 bytes, past the prefix `tap` \
             previews, so its surface id cannot be read from here. Exchange an id from this \
             channel directly: `tatolab exchange <surface-id> --out <dir>`."
        );
    }

    // The channel form.

    #[test]
    fn the_channel_form_taps_then_exchanges_each_sampled_id() {
        let stub_local_api_server = StubLocalApiServer::serve_tapping(
            &[tap_result_text(&[
                bag_publishing_surface_id("s#1"),
                bag_publishing_surface_id("s#2"),
            ])],
            [
                ("s#1", labelled_png_image_answer("one")),
                ("s#2", labelled_png_image_answer("two")),
            ],
        );
        let output_directory = tempfile::tempdir().unwrap();

        let report = sample_the_stub_channel(
            &stub_local_api_server.local_api_socket_path,
            output_directory.path(),
            &sampling_bounds(2, 1),
        );

        let SampledChannelExchangeReport {
            written_image_paths,
            retried_recycled_surface_ids,
            bags_missing_the_surface_id_field,
            bags_examined,
            tap_rounds,
            stopped_early_because,
        } = &report;
        assert_eq!(
            written_image_paths,
            &[
                output_directory.path().join("0000-s_1.png"),
                output_directory.path().join("0001-s_2.png"),
            ]
        );
        assert!(retried_recycled_surface_ids.is_empty());
        assert_eq!(*bags_missing_the_surface_id_field, 0);
        assert_eq!(*bags_examined, 2);
        assert_eq!(*tap_rounds, 1);
        assert!(stopped_early_because.is_none(), "{stopped_early_because:?}");
        assert_eq!(
            written_image_contents(&report),
            [png_bytes_for("one"), png_bytes_for("two")]
        );
        let recorded_tool_calls = stub_local_api_server.recorded_tool_calls();
        assert_eq!(recorded_tool_calls.len(), 1);
        assert_eq!(recorded_tool_calls[0].tool_name, "tap");
        assert_eq!(
            recorded_tool_calls[0].tool_arguments,
            json!({"channel": "cam/frame", "count": 2})
        );
        assert_eq!(
            stub_local_api_server.recorded_image_request_paths(),
            ["/api/surfaces/s%231/image", "/api/surfaces/s%232/image"]
        );
    }

    /// `tap` keeps its contract: it is asked for bags alone, never a field to read.
    #[test]
    fn the_field_override_reads_the_key_the_caller_named_and_tap_is_never_asked_to_read_a_bag() {
        let stub_local_api_server = StubLocalApiServer::serve_tapping(
            &[tap_result_text(&[bag_publishing_surface_id_in_field(
                "s#9",
                "rendered_surface",
            )])],
            [("s#9", labelled_png_image_answer("nine"))],
        );
        let output_directory = tempfile::tempdir().unwrap();

        let report = sample_the_stub_channel(
            &stub_local_api_server.local_api_socket_path,
            output_directory.path(),
            &SampledChannelExchangeBounds {
                surface_id_bag_field_name: "rendered_surface".to_owned(),
                ..sampling_bounds(1, 1)
            },
        );

        assert_eq!(written_image_contents(&report), [png_bytes_for("nine")]);
        assert_eq!(
            stub_local_api_server.recorded_tool_calls()[0].tool_arguments,
            json!({"channel": "cam/frame", "count": 1})
        );
    }

    /// The run recovers and says which id it gave up on, so a sample never quietly becomes a
    /// different frame.
    #[test]
    fn a_recycled_frame_is_retried_against_a_newer_bag_and_reported() {
        let stub_local_api_server = StubLocalApiServer::serve_tapping(
            &[
                tap_result_text(&[bag_publishing_surface_id("stale#1")]),
                tap_result_text(&[bag_publishing_surface_id("fresh#2")]),
            ],
            [
                ("stale#1", recycled_frame_answer()),
                ("fresh#2", labelled_png_image_answer("fresh")),
            ],
        );
        let output_directory = tempfile::tempdir().unwrap();

        let report = sample_the_stub_channel(
            &stub_local_api_server.local_api_socket_path,
            output_directory.path(),
            &sampling_bounds(1, 1),
        );

        assert_eq!(written_image_contents(&report), [png_bytes_for("fresh")]);
        assert_eq!(report.retried_recycled_surface_ids, ["stale#1"]);
        assert_eq!(report.tap_rounds, 2);
        assert_eq!(stop_reason_as_printed(&report), None);
    }

    #[test]
    fn a_channel_whose_frames_always_recycle_gives_up_after_the_round_cap() {
        let stub_local_api_server = StubLocalApiServer::serve(StubLocalApiScript {
            fixed_tool_answer: Some(StubToolAnswer::tool_result(&tap_result_text(&[
                bag_publishing_surface_id("stale#1"),
            ]))),
            surface_image_answers: surface_image_answers_by_id([(
                "stale#1",
                recycled_frame_answer(),
            )]),
            ..StubLocalApiScript::default()
        });
        let output_directory = tempfile::tempdir().unwrap();

        let report = sample_the_stub_channel(
            &stub_local_api_server.local_api_socket_path,
            output_directory.path(),
            &sampling_bounds(1, 1),
        );

        assert_eq!(report.tap_rounds, MAX_TAP_ROUNDS_PER_SAMPLE_RUN);
        assert_eq!(report.retried_recycled_surface_ids, ["stale#1"; 8]);
        assert!(report.written_image_paths.is_empty());
        assert_eq!(stop_reason_as_printed(&report), None);
    }

    #[test]
    fn a_bag_without_the_named_field_is_counted_rather_than_fatal() {
        let stub_local_api_server = StubLocalApiServer::serve_tapping(
            &[tap_result_text(&[
                bag_publishing_no_surface_id(),
                bag_publishing_surface_id("s#1"),
            ])],
            [("s#1", labelled_png_image_answer("one"))],
        );
        let output_directory = tempfile::tempdir().unwrap();

        let report = sample_the_stub_channel(
            &stub_local_api_server.local_api_socket_path,
            output_directory.path(),
            &sampling_bounds(1, 1),
        );

        assert_eq!(written_image_contents(&report), [png_bytes_for("one")]);
        assert_eq!(report.bags_missing_the_surface_id_field, 1);
    }

    #[test]
    fn every_nth_bag_selects_the_stride_and_asks_for_enough_bags_to_fill_it() {
        let labels = ["a", "b", "c", "d", "e", "f"];
        let stub_local_api_server = StubLocalApiServer::serve_tapping(
            &[tap_result_text(&labels.map(|label| {
                bag_publishing_surface_id(&format!("s#{label}"))
            }))],
            labels.map(|label| (format!("s#{label}"), labelled_png_image_answer(label))),
        );
        let output_directory = tempfile::tempdir().unwrap();

        let report = sample_the_stub_channel(
            &stub_local_api_server.local_api_socket_path,
            output_directory.path(),
            &sampling_bounds(2, 3),
        );

        assert_eq!(
            written_image_contents(&report),
            [png_bytes_for("a"), png_bytes_for("d")]
        );
        assert_eq!(
            stub_local_api_server.recorded_tool_calls()[0].tool_arguments["count"],
            6
        );
    }

    /// A stride restarted per round would exchange `a` then `c`, the first bag of each round.
    #[test]
    fn the_stride_runs_across_tap_rounds_on_one_client_rather_than_restarting() {
        let stub_local_api_server = StubLocalApiServer::serve_tapping(
            &[
                tap_result_text(
                    &["a", "b"].map(|label| bag_publishing_surface_id(&format!("s#{label}"))),
                ),
                tap_result_text(
                    &["c", "d"].map(|label| bag_publishing_surface_id(&format!("s#{label}"))),
                ),
            ],
            ["a", "b", "c", "d"]
                .map(|label| (format!("s#{label}"), labelled_png_image_answer(label))),
        );
        let output_directory = tempfile::tempdir().unwrap();

        let report = sample_the_stub_channel(
            &stub_local_api_server.local_api_socket_path,
            output_directory.path(),
            &sampling_bounds(2, 3),
        );

        assert_eq!(
            written_image_contents(&report),
            [png_bytes_for("a"), png_bytes_for("d")],
            "the stride restarted at each tap round"
        );
        assert_eq!(report.tap_rounds, 2);
        assert_eq!(report.bags_examined, 4);
        assert_eq!(
            stub_local_api_server
                .recorded_tool_calls()
                .iter()
                .map(|recorded_tool_call| recorded_tool_call.tool_arguments["count"].clone())
                .collect::<Vec<_>>(),
            [json!(6), json!(3)]
        );
    }

    /// The exchanges of every round, a recycled frame's among them, ride the one connection the
    /// run opened, never a connect per frame.
    #[test]
    fn every_exchange_of_a_run_rides_one_local_api_connection() {
        let stub_local_api_server = StubLocalApiServer::serve_tapping(
            &[
                tap_result_text(&[
                    bag_publishing_surface_id("s#1"),
                    bag_publishing_surface_id("stale#2"),
                ]),
                tap_result_text(&[bag_publishing_surface_id("s#3")]),
            ],
            [
                ("s#1", labelled_png_image_answer("one")),
                ("stale#2", recycled_frame_answer()),
                ("s#3", labelled_png_image_answer("three")),
            ],
        );
        let output_directory = tempfile::tempdir().unwrap();

        let report = sample_the_stub_channel(
            &stub_local_api_server.local_api_socket_path,
            output_directory.path(),
            &sampling_bounds(2, 1),
        );

        assert_eq!(
            written_image_contents(&report),
            [png_bytes_for("one"), png_bytes_for("three")]
        );
        assert_eq!(report.tap_rounds, 2);
        assert_eq!(
            stub_local_api_server.recorded_image_request_paths().len(),
            3
        );
        assert_eq!(stub_local_api_server.image_request_connection_count(), 1);
    }

    /// The one frame that landed is still named, and the run says it fell short.
    #[test]
    fn a_short_sample_reports_what_landed_over_every_round_it_spent() {
        let stub_local_api_server = StubLocalApiServer::serve_tapping(
            &[tap_result_text(&[bag_publishing_surface_id("s#1")])],
            [("s#1", labelled_png_image_answer("one"))],
        );
        let output_directory = tempfile::tempdir().unwrap();

        let report = sample_the_stub_channel(
            &stub_local_api_server.local_api_socket_path,
            output_directory.path(),
            &sampling_bounds(3, 1),
        );

        assert_eq!(written_image_contents(&report), [png_bytes_for("one")]);
        assert_eq!(report.tap_rounds, MAX_TAP_ROUNDS_PER_SAMPLE_RUN);
        assert_eq!(stop_reason_as_printed(&report), None);
        assert!(
            render_sampled_channel_exchange_report(FIXTURE_CHANNEL, &report, 3)
                .starts_with("exchanged 1 of 3 requested frames from `cam/frame`")
        );
    }

    #[test]
    fn a_refusal_that_cannot_be_retried_stops_the_run() {
        let stub_local_api_server = StubLocalApiServer::serve_tapping(
            &[tap_result_text(&[bag_publishing_surface_id("s#1")])],
            [(
                "s#1",
                StubSurfaceImageAnswer::refusal(501, "no conversion arm"),
            )],
        );
        let output_directory = tempfile::tempdir().unwrap();

        let report = sample_the_stub_channel(
            &stub_local_api_server.local_api_socket_path,
            output_directory.path(),
            &sampling_bounds(1, 1),
        );

        assert!(report.written_image_paths.is_empty());
        assert_eq!(report.tap_rounds, 1);
        assert_eq!(
            stop_reason_as_printed(&report).as_deref(),
            Some("exchange of surface `s#1` answered 501: no conversion arm")
        );
    }

    /// Every PNG on disk is a PNG that was named, the stop reported beside the frames.
    #[test]
    fn frames_that_landed_before_a_fatal_stop_are_still_reported() {
        let stub_local_api_server = StubLocalApiServer::serve_tapping(
            &[tap_result_text(&[
                bag_publishing_surface_id("s#1"),
                bag_publishing_surface_id("s#2"),
            ])],
            [
                ("s#1", labelled_png_image_answer("one")),
                (
                    "s#2",
                    StubSurfaceImageAnswer::refusal(404, "no such surface"),
                ),
            ],
        );
        let output_directory = tempfile::tempdir().unwrap();

        let report = sample_the_stub_channel(
            &stub_local_api_server.local_api_socket_path,
            output_directory.path(),
            &sampling_bounds(2, 1),
        );

        assert_eq!(written_image_contents(&report), [png_bytes_for("one")]);
        assert_eq!(
            stop_reason_as_printed(&report).as_deref(),
            Some("exchange of surface `s#2` answered 404: no such surface")
        );
        assert_eq!(png_files_in(output_directory.path()), 1);
    }

    /// A preview cut short with no cap flagged still decodes to a refusal naming the truncation,
    /// never a bag missing its later fields.
    #[test]
    fn a_bag_the_tap_truncated_stops_the_run_by_name() {
        let whole_bag = framed_bag(
            &msgpack_named_map(&[
                ("surface_id", "s#1".into()),
                ("filler", "x".repeat(200).into()),
            ]),
            SLICE_HOLDS_ONLY_THE_BAG,
        );
        let stub_local_api_server = StubLocalApiServer::serve_tapping(
            &[tap_result_text(&[
                whole_bag[..whole_bag.len() - 32].to_vec()
            ])],
            Vec::<(String, StubSurfaceImageAnswer)>::new(),
        );
        let output_directory = tempfile::tempdir().unwrap();

        let report = sample_the_stub_channel(
            &stub_local_api_server.local_api_socket_path,
            output_directory.path(),
            &sampling_bounds(1, 1),
        );

        let stop_reason = stop_reason_as_printed(&report).unwrap();
        assert!(stop_reason.contains("truncated"), "{stop_reason}");
    }

    /// Counting a capped bag as one with no surface id would blame the channel for the tool's own
    /// limit, and retrying it would never converge; the id form still reaches its frame.
    #[test]
    fn a_bag_past_the_taps_preview_cap_stops_the_run_and_names_the_size() {
        let stub_local_api_server = StubLocalApiServer::serve_tapping(
            &[tap_result_text_capping_bags(
                &[bag_publishing_surface_id("s#1")],
                &[0],
            )],
            [("s#1", labelled_png_image_answer("one"))],
        );
        let output_directory = tempfile::tempdir().unwrap();

        let report = sample_the_stub_channel(
            &stub_local_api_server.local_api_socket_path,
            output_directory.path(),
            &sampling_bounds(1, 1),
        );

        assert!(
            matches!(
                &report.stopped_early_because,
                Some(SampledChannelExchangeStop::SelectedBagPastTheTapPreviewCap {
                    channel,
                    whole_bag_byte_len: CAPPED_BAG_STATED_BYTE_LEN,
                }) if channel == FIXTURE_CHANNEL
            ),
            "{:?}",
            report.stopped_early_because
        );
        assert!(
            stub_local_api_server
                .recorded_image_request_paths()
                .is_empty()
        );
    }

    /// Bag 0 is selected and publishes no id, so the loop must reach the capped bag 1 and pass it
    /// by for the run to finish on bag 2.
    #[test]
    fn the_stride_steps_over_an_oversized_bag_rather_than_dying_on_it() {
        let stub_local_api_server = StubLocalApiServer::serve_tapping(
            &[tap_result_text_capping_bags(
                &[
                    bag_publishing_no_surface_id(),
                    bag_publishing_surface_id("s#2"),
                    bag_publishing_surface_id("s#3"),
                ],
                &[1],
            )],
            [("s#3", labelled_png_image_answer("three"))],
        );
        let output_directory = tempfile::tempdir().unwrap();

        let report = sample_the_stub_channel(
            &stub_local_api_server.local_api_socket_path,
            output_directory.path(),
            &sampling_bounds(1, 2),
        );

        assert_eq!(
            stop_reason_as_printed(&report),
            None,
            "a capped bag the stride skipped ended a run that never needed it"
        );
        assert_eq!(written_image_contents(&report), [png_bytes_for("three")]);
        assert_eq!(report.bags_missing_the_surface_id_field, 1);
    }

    #[test]
    fn a_bag_the_stride_skips_cannot_kill_the_run_by_being_oversized() {
        let stub_local_api_server = StubLocalApiServer::serve_tapping(
            &[tap_result_text_capping_bags(
                &[
                    bag_publishing_surface_id("s#1"),
                    bag_publishing_surface_id("s#2"),
                ],
                &[1],
            )],
            [("s#1", labelled_png_image_answer("one"))],
        );
        let output_directory = tempfile::tempdir().unwrap();

        let report = sample_the_stub_channel(
            &stub_local_api_server.local_api_socket_path,
            output_directory.path(),
            &sampling_bounds(1, 2),
        );

        assert_eq!(stop_reason_as_printed(&report), None);
        assert_eq!(written_image_contents(&report), [png_bytes_for("one")]);
    }

    /// Failing the whole tap round on bag 1 would throw away bag 0's frame, already exchanged.
    #[test]
    fn an_oversized_bag_does_not_discard_the_readable_bags_beside_it() {
        let stub_local_api_server = StubLocalApiServer::serve_tapping(
            &[tap_result_text_capping_bags(
                &[
                    bag_publishing_surface_id("s#1"),
                    bag_publishing_surface_id("s#2"),
                ],
                &[1],
            )],
            [("s#1", labelled_png_image_answer("one"))],
        );
        let output_directory = tempfile::tempdir().unwrap();

        let report = sample_the_stub_channel(
            &stub_local_api_server.local_api_socket_path,
            output_directory.path(),
            &sampling_bounds(2, 1),
        );

        assert_eq!(written_image_contents(&report), [png_bytes_for("one")]);
        let stop_reason = stop_reason_as_printed(&report).unwrap();
        assert!(stop_reason.contains("9000 bytes"), "{stop_reason}");
    }

    /// The second frame's file name is already a directory, so its write fails after the first
    /// frame landed.
    #[test]
    fn a_write_that_fails_still_names_the_frames_that_landed() {
        let stub_local_api_server = StubLocalApiServer::serve_tapping(
            &[tap_result_text(&[
                bag_publishing_surface_id("s#1"),
                bag_publishing_surface_id("s#2"),
            ])],
            [
                ("s#1", labelled_png_image_answer("one")),
                ("s#2", labelled_png_image_answer("two")),
            ],
        );
        let output_directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(output_directory.path().join("0001-s_2.png")).unwrap();

        let report = sample_the_stub_channel(
            &stub_local_api_server.local_api_socket_path,
            output_directory.path(),
            &sampling_bounds(2, 1),
        );

        assert_eq!(written_image_contents(&report), [png_bytes_for("one")]);
        let stop_reason = stop_reason_as_printed(&report).unwrap();
        assert!(
            stop_reason.starts_with(&format!(
                "could not write into `{}`: ",
                output_directory.path().display()
            )),
            "{stop_reason}"
        );
    }

    #[test]
    fn a_tap_the_runtime_fails_or_refuses_stops_the_run_in_its_first_round() {
        let failing_stub_local_api_server =
            StubLocalApiServer::serve_answering_every_tool_call_with(StubToolAnswer::tool_failure(
                "no such channel",
            ));
        let refusing_stub_local_api_server = StubLocalApiServer::serve(StubLocalApiScript {
            refuse_every_tool_call_with: Some("channel must name a port".to_owned()),
            ..StubLocalApiScript::default()
        });
        let output_directory = tempfile::tempdir().unwrap();

        for (stub_local_api_server, stop_reason) in [
            (
                &failing_stub_local_api_server,
                "tap failed: no such channel",
            ),
            (
                &refusing_stub_local_api_server,
                "tap failed: channel must name a port (-32602)",
            ),
        ] {
            let report = sample_the_stub_channel(
                &stub_local_api_server.local_api_socket_path,
                output_directory.path(),
                &sampling_bounds(1, 1),
            );

            assert_eq!(report.tap_rounds, 1);
            assert_eq!(
                stop_reason_as_printed(&report).as_deref(),
                Some(stop_reason)
            );
        }
    }

    #[test]
    fn a_tap_nothing_answers_stops_the_run_in_its_first_round() {
        let output_directory = tempfile::tempdir().unwrap();

        let report = sample_the_stub_channel(
            Path::new(NOTHING_LISTENS_LOCAL_API_SOCKET_PATH),
            output_directory.path(),
            &sampling_bounds(1, 1),
        );

        assert_eq!(report.tap_rounds, 1);
        let stop_reason = stop_reason_as_printed(&report).unwrap();
        assert!(
            stop_reason.starts_with(&format!(
                "no runtime answers MCP at {NOTHING_LISTENS_LOCAL_API_SOCKET_PATH} ("
            )),
            "{stop_reason}"
        );
    }

    // The report.

    #[test]
    fn the_report_names_every_count_in_the_plural() {
        let report = SampledChannelExchangeReport {
            written_image_paths: vec![PathBuf::from("frames/0000-s_1.png")],
            retried_recycled_surface_ids: vec!["s#2".to_owned(), "s#3".to_owned()],
            bags_missing_the_surface_id_field: 4,
            bags_examined: 9,
            tap_rounds: 3,
            stopped_early_because: Some(SampledChannelExchangeStop::SurfaceImageExchangeFailed(
                SurfaceImageExchangeFailure::RefusedByTheRuntime {
                    published_surface_id: "s#4".to_owned(),
                    http_status: StatusCode::NOT_IMPLEMENTED,
                    refusal_detail: String::new(),
                },
            )),
        };

        assert_eq!(
            render_sampled_channel_exchange_report("cam/frame", &report, 3),
            "exchanged 1 of 3 requested frames from `cam/frame` (9 bags examined over 3 tap \
             rounds)\n\
             retried 2 recycled frames against newer bags: s#2, s#3\n\
             4 bags carried no surface id in the named field — name the right one with \
             `--field`\n\
             error: exchange of surface `s#4` answered 501\n"
        );
    }

    #[test]
    fn the_report_names_a_count_of_one_in_the_singular_and_leaves_out_what_did_not_happen() {
        let retried_and_missing_one = SampledChannelExchangeReport {
            retried_recycled_surface_ids: vec!["s#2".to_owned()],
            bags_missing_the_surface_id_field: 1,
            bags_examined: 1,
            tap_rounds: 1,
            ..SampledChannelExchangeReport::default()
        };

        assert_eq!(
            render_sampled_channel_exchange_report("cam/frame", &retried_and_missing_one, 1),
            "exchanged 0 of 1 requested frames from `cam/frame` (1 bags examined over 1 tap \
             round)\n\
             retried 1 recycled frame against newer bags: s#2\n\
             1 bag carried no surface id in the named field — name the right one with \
             `--field`\n"
        );
        assert_eq!(
            render_sampled_channel_exchange_report(
                "cam/frame",
                &SampledChannelExchangeReport {
                    written_image_paths: vec![PathBuf::from("frames/0000-s_1.png")],
                    bags_examined: 1,
                    tap_rounds: 1,
                    ..SampledChannelExchangeReport::default()
                },
                1
            ),
            "exchanged 1 of 1 requested frames from `cam/frame` (1 bags examined over 1 tap \
             round)\n"
        );
    }
}

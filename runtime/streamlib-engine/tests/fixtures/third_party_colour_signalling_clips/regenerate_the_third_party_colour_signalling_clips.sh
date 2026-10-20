#!/usr/bin/env bash
# Regenerates the one-access-unit clips `vulkan_video_decodes_with_the_colour_a_stream_signals`
# decodes: five RGB bars encoded by ffmpeg's Vulkan Video encoders, each clip's samples
# in one matrix and range and its VUI signalling that colour, or none at all.
#
# Usage: regenerate_the_third_party_colour_signalling_clips.sh
# Needs an ffmpeg with h264_vulkan and hevc_vulkan, on a device that encodes Vulkan Video.
set -euo pipefail

clip_directory=$(cd "$(dirname "$0")" && pwd)
bar_picture_directory=$(mktemp -d)
trap 'rm -rf "$bar_picture_directory"' EXIT

# The bars the test holds each decoded picture to, in order: red, green, blue, light
# grey, near black. None of them clips under either range.
write_bar_picture() {
  local width=$1 height=$2 bar_width=$(($1 / 5))
  local bar
  local filter=""
  local labels=""
  local index=0
  for bar in 0xC02020 0x20C020 0x2020C0 0xC8C8C8 0x101010; do
    filter+="color=c=${bar}:s=${bar_width}x${height}:d=1,format=rgb24[bar${index}];"
    labels+="[bar${index}]"
    index=$((index + 1))
  done
  ffmpeg -hide_banner -loglevel error -y -f lavfi -i "${filter}${labels}hstack=5" \
    -frames:v 1 "$bar_picture_directory/bars_${width}x${height}.png"
}

# The VUI ffmpeg writes is the frames' own colour properties, so `setparams` sets what
# the clip signals independently of the matrix and range `scale` writes the samples in.
write_clip() {
  local clip_name=$1 codec=$2 extent=$3 sample_matrix=$4 sample_range=$5 signalled_colour=$6
  ffmpeg -hide_banner -loglevel error -y -init_hw_device vulkan=vulkan_device:0 \
    -filter_hw_device vulkan_device \
    -i "$bar_picture_directory/bars_${extent}.png" \
    -vf "scale=out_color_matrix=${sample_matrix}:out_range=${sample_range},format=nv12,setparams=${signalled_colour},hwupload" \
    -frames:v 1 -c:v "${codec}_vulkan" -f "$codec" "$clip_directory/${clip_name}.annexb"
}

signals_bt709_full_range=range=pc:color_primaries=bt709:color_trc=bt709:colorspace=bt709
signals_bt601_limited_range=range=tv:color_primaries=smpte170m:color_trc=smpte170m:colorspace=smpte170m
signals_nothing=range=unknown:color_primaries=unknown:color_trc=unknown:colorspace=unknown

write_bar_picture 1280 720
write_bar_picture 640 480

write_clip h264_bt709_full_range_1280x720 h264 1280x720 bt709 pc "$signals_bt709_full_range"
write_clip h264_bt601_limited_range_1280x720 h264 1280x720 bt601 tv "$signals_bt601_limited_range"
write_clip h264_unsignalled_1280x720 h264 1280x720 bt709 tv "$signals_nothing"
write_clip h264_unsignalled_640x480 h264 640x480 bt601 tv "$signals_nothing"
write_clip h265_bt709_full_range_1280x720 hevc 1280x720 bt709 pc "$signals_bt709_full_range"
write_clip h265_bt601_limited_range_1280x720 hevc 1280x720 bt601 tv "$signals_bt601_limited_range"

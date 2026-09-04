// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Reading the Opus media parameters out of an SDP answer.
//!
//! The rtpmap is no use for this: RFC 7587 §7 fixes Opus's encoding parameter
//! at 2 in every `a=rtpmap` ever negotiated, mono streams included. What the
//! sender actually declares rides the fmtp instead.

/// The Opus fmtp's `sprop-stereo`, the sender's own hint about what it will
/// send. `None` when the answer describes no Opus fmtp at all; RFC 7587 §3.1.1
/// makes the parameter default to mono when the line exists without it.
///
/// A hint, not the authority: the per-packet TOC byte is what a decoder reads,
/// and this exists to be checked against it.
pub(crate) fn opus_sender_stereo_hint_in_answer(session_description: &str) -> Option<bool> {
    let fmtp = opus_format_parameters_in_answer(session_description)?;
    Some(
        fmtp.split(';')
            .filter_map(|parameter| parameter.trim().split_once('='))
            .find(|(name, _)| *name == "sprop-stereo")
            .is_some_and(|(_, value)| value.trim() == "1"),
    )
}

/// The parameter list off the `a=fmtp:` line belonging to the answer's Opus
/// payload type, found by way of its `a=rtpmap:`.
fn opus_format_parameters_in_answer(session_description: &str) -> Option<&str> {
    let payload_type = session_description.lines().find_map(|line| {
        let mapping = line.trim().strip_prefix("a=rtpmap:")?;
        let (payload_type, codec) = mapping.split_once(char::is_whitespace)?;
        codec
            .to_ascii_lowercase()
            .starts_with("opus/")
            .then_some(payload_type)
    })?;

    session_description.lines().find_map(|line| {
        let parameters = line.trim().strip_prefix("a=fmtp:")?;
        let (declared_for, parameters) = parameters.split_once(char::is_whitespace)?;
        (declared_for == payload_type).then_some(parameters)
    })
}

/// The Opus fmtp a publisher offers. `sprop-stereo` states what this sender
/// will send; `minptime` and `useinbandfec` are what every WHIP relay expects.
pub(crate) fn opus_format_parameters_for_offer(channels: u32) -> String {
    format!(
        "minptime=10;useinbandfec=1;sprop-stereo={}",
        u8::from(channels > 1)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const STEREO_ANSWER: &str = "v=0\r\n\
         m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
         a=rtpmap:111 opus/48000/2\r\n\
         a=fmtp:111 minptime=10;useinbandfec=1;sprop-stereo=1\r\n";

    const MONO_ANSWER: &str = "v=0\r\n\
         m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
         a=rtpmap:111 opus/48000/2\r\n\
         a=fmtp:111 minptime=10;useinbandfec=1\r\n";

    #[test]
    fn the_stereo_hint_comes_off_the_fmtp() {
        assert_eq!(opus_sender_stereo_hint_in_answer(STEREO_ANSWER), Some(true));
    }

    #[test]
    fn an_fmtp_without_the_parameter_means_mono_not_unknown() {
        assert_eq!(opus_sender_stereo_hint_in_answer(MONO_ANSWER), Some(false));
    }

    #[test]
    fn the_rtpmaps_channel_field_is_two_even_for_the_mono_answer() {
        assert!(MONO_ANSWER.contains("opus/48000/2"));
        assert_eq!(opus_sender_stereo_hint_in_answer(MONO_ANSWER), Some(false));
    }

    #[test]
    fn an_answer_with_no_opus_at_all_declares_nothing() {
        let video_only = "v=0\r\n\
             m=video 9 UDP/TLS/RTP/SAVPF 102\r\n\
             a=rtpmap:102 H264/90000\r\n";

        assert_eq!(opus_sender_stereo_hint_in_answer(video_only), None);
    }

    #[test]
    fn the_fmtp_is_matched_to_opus_by_payload_type_not_by_position() {
        let two_codecs = "v=0\r\n\
             m=audio 9 UDP/TLS/RTP/SAVPF 8 111\r\n\
             a=rtpmap:8 PCMA/8000\r\n\
             a=fmtp:8 sprop-stereo=1\r\n\
             a=rtpmap:111 opus/48000/2\r\n\
             a=fmtp:111 minptime=10\r\n";

        assert_eq!(opus_sender_stereo_hint_in_answer(two_codecs), Some(false));
    }

    #[test]
    fn an_opus_rtpmap_with_no_fmtp_line_declares_nothing() {
        let no_fmtp = "v=0\r\n\
             m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
             a=rtpmap:111 opus/48000/2\r\n";

        assert_eq!(opus_sender_stereo_hint_in_answer(no_fmtp), None);
    }

    #[test]
    fn the_offered_parameters_state_what_this_sender_will_send() {
        assert!(opus_format_parameters_for_offer(2).ends_with("sprop-stereo=1"));
        assert!(opus_format_parameters_for_offer(1).ends_with("sprop-stereo=0"));
    }
}


use crate::core::AudioFrame;

pub(crate) fn deinterleave_audio_frame(frame: &AudioFrame) -> Vec<Vec<f32>> {
    let num_channels = frame.channels as usize;
    let num_samples = frame.sample_count();

    let mut channels: Vec<Vec<f32>> = (0..num_channels)
        .map(|_| Vec::with_capacity(num_samples))
        .collect();

    for chunk in frame.samples.chunks_exact(num_channels) {
        for (ch_idx, sample) in chunk.iter().enumerate() {
            channels[ch_idx].push(*sample);
        }
    }

    channels
}

pub(crate) fn interleave_to_audio_frame(
    channel_buffers: &[Vec<f32>],
    timestamp_ns: i64,
    frame_number: u64,
) -> AudioFrame {
    assert!(!channel_buffers.is_empty(), "Must have at least one channel");

    let num_channels = channel_buffers.len();
    let num_samples = channel_buffers[0].len();

    for (i, buf) in channel_buffers.iter().enumerate() {
        assert_eq!(buf.len(), num_samples, "Channel {} has different length", i);
    }

    let mut samples = Vec::with_capacity(num_samples * num_channels);

    for sample_idx in 0..num_samples {
        for ch_buf in channel_buffers.iter() {
            samples.push(ch_buf[sample_idx]);
        }
    }

    AudioFrame::new(samples, timestamp_ns, frame_number, num_channels as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deinterleave_audio_frame_stereo() {
        let frame = AudioFrame::new(
            vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0],
            0,
            0,
            2,
        );

        let channels = deinterleave_audio_frame(&frame);

        assert_eq!(channels.len(), 2);
        assert_eq!(channels[0], vec![1.0, 3.0, 5.0, 7.0]);
        assert_eq!(channels[1], vec![2.0, 4.0, 6.0, 8.0]);
    }

    #[test]
    fn test_deinterleave_audio_frame_quad() {
        let frame = AudioFrame::new(
            vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0],
            0,
            0,
            4,
        );

        let channels = deinterleave_audio_frame(&frame);

        assert_eq!(channels.len(), 4);
        assert_eq!(channels[0], vec![1.0, 5.0]);
        assert_eq!(channels[1], vec![2.0, 6.0]);
        assert_eq!(channels[2], vec![3.0, 7.0]);
        assert_eq!(channels[3], vec![4.0, 8.0]);
    }

    #[test]
    fn test_interleave_to_audio_frame_stereo() {
        let left = vec![1.0, 3.0, 5.0, 7.0];
        let right = vec![2.0, 4.0, 6.0, 8.0];

        let frame = interleave_to_audio_frame(&[left, right], 1000, 1);

        assert_eq!(frame.channels, 2);
        assert_eq!(frame.samples.len(), 8);
        assert_eq!(*frame.samples, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
    }

    #[test]
    fn test_interleave_to_audio_frame_5_1() {
        let ch1 = vec![1.0, 7.0];
        let ch2 = vec![2.0, 8.0];
        let ch3 = vec![3.0, 9.0];
        let ch4 = vec![4.0, 10.0];
        let ch5 = vec![5.0, 11.0];
        let ch6 = vec![6.0, 12.0];

        let frame = interleave_to_audio_frame(&[ch1, ch2, ch3, ch4, ch5, ch6], 1000, 1);

        assert_eq!(frame.channels, 6);
        assert_eq!(frame.samples.len(), 12);
        assert_eq!(*frame.samples, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0]);
    }

    #[test]
    fn test_roundtrip_conversion() {
        let original = AudioFrame::new(
            vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6],
            5000,
            42,
            2,
        );

        let channels = deinterleave_audio_frame(&original);
        let roundtrip = interleave_to_audio_frame(
            &channels,
            original.timestamp_ns,
            original.frame_number,
        );

        assert_eq!(*original.samples, *roundtrip.samples);
        assert_eq!(original.timestamp_ns, roundtrip.timestamp_ns);
        assert_eq!(original.frame_number, roundtrip.frame_number);
    }
}

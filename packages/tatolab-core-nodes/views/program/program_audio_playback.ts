// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

import type { PortViewerMessageReading } from "@tatolab/tato-app-view-sdk";

/** A Channels reading, which carries the block's samples. */
export type ChannelsReading = Extract<PortViewerMessageReading, { family: "channels" }>;

// How far ahead of the audio clock a block is scheduled, so a block that
// reaches the view late still plays on time.
const LEAD_SECONDS = 0.08;
// A queue that falls behind the clock, or runs further ahead than this, as the
// stream's clock and the sound card's drift apart, starts again from the lead.
const MOST_LEAD_SECONDS = 0.4;

/** Plays each Channels block Studio hands the view, one after the other with no gap, on the default output device. */
export class ProgramAudioPlayback {
  private audioContext: AudioContext | null = null;
  private nextBlockStartsAt = 0;

  /** Schedule `reading`'s samples after the block before. */
  play(reading: ChannelsReading): void {
    const channelSamples = reading.channel_samples;
    if (channelSamples.length === 0 || reading.samples_per_channel === 0) return;
    const audioContext = this.theAudioContext();
    const block = audioContext.createBuffer(channelSamples.length, reading.samples_per_channel, reading.sample_rate);
    channelSamples.forEach((samples, channel) => block.getChannelData(channel).set(samples));
    const now = audioContext.currentTime;
    if (this.nextBlockStartsAt < now || this.nextBlockStartsAt > now + MOST_LEAD_SECONDS) {
      this.nextBlockStartsAt = now + LEAD_SECONDS;
    }
    const blockSource = audioContext.createBufferSource();
    blockSource.buffer = block;
    blockSource.connect(audioContext.destination);
    blockSource.start(this.nextBlockStartsAt);
    this.nextBlockStartsAt += block.duration;
  }

  /** Stop what is scheduled and let the output device go, until the next block. */
  stop(): void {
    if (this.audioContext === null) return;
    void this.audioContext.close();
    this.audioContext = null;
  }

  private theAudioContext(): AudioContext {
    if (this.audioContext === null) {
      this.audioContext = new AudioContext({ latencyHint: "interactive" });
      this.nextBlockStartsAt = 0;
    }
    // Chromium starts a context suspended until the page has seen a gesture;
    // it resumes on the next one.
    if (this.audioContext.state === "suspended") void this.audioContext.resume();
    return this.audioContext;
  }
}

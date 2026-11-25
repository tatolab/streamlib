// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

// The music wired into `audio`, played on the default output device: each block scheduled a little
// ahead of the sound card's clock, the queue starting again from there when it falls behind or
// drifts too far ahead.

import type { PortViewerMessageReading } from "@tatolab/tato-app-view-sdk";

/** How far ahead of the sound card's clock a block is scheduled. */
const AUDIO_LEAD_SECONDS = 0.08;
/** The furthest ahead the queue runs before it starts again from the lead. */
const AUDIO_MOST_LEAD_SECONDS = 0.4;

/** One Channels block a port viewer read. */
type ChannelsBlock = Extract<PortViewerMessageReading, { message: "channels_block" }>;

/** The sound the stage plays, muted or not; it opens its audio context on the first block it plays. */
export class StageSound {
  private context: AudioContext | null = null;
  private gain: GainNode | null = null;
  private playsAt = 0;
  private muted = false;

  /** Whether the stage is muted. */
  get isMuted(): boolean {
    return this.muted;
  }

  /** Mute or unmute; muting closes the sound until the next block after unmuting. */
  setMuted(muted: boolean): void {
    this.muted = muted;
    if (muted) this.stop();
  }

  /** Schedule `block` after the blocks before it. */
  play(block: ChannelsBlock): void {
    const channels = block.channel_samples;
    if (this.muted || channels.length === 0 || block.samples_per_channel === 0) return;
    const { context, gain } = this.opened();
    if (context.state === "suspended") void context.resume();
    const buffer = context.createBuffer(channels.length, block.samples_per_channel, block.sample_rate);
    channels.forEach((samples, channel) => buffer.getChannelData(channel).set(samples));
    const now = context.currentTime;
    if (this.playsAt < now || this.playsAt > now + AUDIO_MOST_LEAD_SECONDS) this.playsAt = now + AUDIO_LEAD_SECONDS;
    const source = context.createBufferSource();
    source.buffer = buffer;
    source.connect(gain);
    source.start(this.playsAt);
    this.playsAt += buffer.duration;
  }

  /** Resume a sound the page has kept suspended until it saw a gesture. */
  resumeAfterAGesture(): void {
    if (this.context?.state === "suspended") void this.context.resume();
  }

  /** Close the sound; the next block opens it again. */
  stop(): void {
    if (this.context === null) return;
    void this.context.close();
    this.context = null;
    this.gain = null;
  }

  private opened(): { context: AudioContext; gain: GainNode } {
    if (this.context !== null && this.gain !== null) return { context: this.context, gain: this.gain };
    const context = new AudioContext({ latencyHint: "interactive" });
    const gain = context.createGain();
    gain.connect(context.destination);
    this.context = context;
    this.gain = gain;
    this.playsAt = 0;
    return { context, gain };
  }
}

// The ♩ music's audio-thread end: plays the chunks of interleaved stereo frames the audio worker renders, and asks it
// for the next as the queue drains, over their own MessageChannel. No timer is on this path, so a background tab's
// throttling never reaches it.
'use strict';

// Frames per chunk: few enough messages that the worklet's garbage stays small (Chrome's "Audio worklet design
// pattern" names MessagePort's "repetitive memory allocation" as its cost).
const CHUNK_FRAMES = 4096;
// How far ahead the queue holds music: a worker stalled this long still plays through.
const AHEAD_S = 1;

class MusicPlayer extends AudioWorkletProcessor {
  constructor() {
    super();
    this.chunks = [];
    this.at = 0;
    this.queued = 0;
    this.asked = 0;
    this.source = null;
    this.port.onmessage = (e) => {
      this.source = e.data.source;
      this.source.onmessage = (m) => this.take(m.data);
      this.ask();
    };
  }

  take(chunk) {
    const frames = chunk.length / 2;
    this.chunks.push(chunk);
    this.queued += frames;
    this.asked -= frames;
    if (!this.primed) {
      this.primed = true;
      this.port.postMessage({ primed: true });
    }
  }

  ask() {
    if (!this.source) return;
    while (this.queued + this.asked + CHUNK_FRAMES <= AHEAD_S * sampleRate) {
      this.source.postMessage({ need: CHUNK_FRAMES });
      this.asked += CHUNK_FRAMES;
    }
  }

  process(_inputs, outputs) {
    const [left, right] = outputs[0];
    for (let i = 0; i < left.length; i++) {
      const chunk = this.chunks[0];
      if (!chunk) {
        // An underrun plays silence; the stream picks up where it was, since lofi reads no clock.
        left[i] = 0;
        right[i] = 0;
        continue;
      }
      left[i] = chunk[this.at];
      right[i] = chunk[this.at + 1];
      this.at += 2;
      this.queued--;
      if (this.at === chunk.length) {
        this.chunks.shift();
        this.at = 0;
        // Back to the worker, which renders into it again: steady state allocates no buffer.
        this.source.postMessage({ spent: chunk }, [chunk.buffer]);
      }
    }
    this.ask();
    // MDN, Using AudioWorklet: "you must always return true from process(), at least on Chrome".
    return true;
  }
}

registerProcessor('pix-music', MusicPlayer);

// The ♩ music OFF the main thread: this worker loads its OWN instance of the
// site's wasm build (memories can't be shared with the page's instance) and
// owns the office's lofi stream for the page's life, rendering each chunk the
// music worklet asks for over the port it was handed. Any message back to the
// main thread means the music failed.
'use strict';

// The stream once built, and the controls that arrived while it was building.
let stream = null;
let asked = null;

self.onmessage = function (e) {
  if (e.data.port) {
    start(e.data);
    return;
  }
  if (stream) stream.set_controls(new Float32Array(e.data.controls));
  else asked = e.data.controls;
};

// `controls` are the office's first, as a tick's `music.controls`: they pick the first song, so the stream is built
// with them before it renders.
async function start({ wasmJsUrl, port, nowMs, controls }) {
  let w;
  try {
    const mod = await import(wasmJsUrl);
    w = await mod.default();
    stream = new mod.MusicStream(nowMs, new Float32Array(controls));
  } catch (err) {
    self.postMessage({ ok: false });
    self.close();
    return;
  }
  if (asked) stream.set_controls(new Float32Array(asked));
  const spare = [];
  port.onmessage = function (e) {
    if (e.data.spent) {
      spare.push(e.data.spent);
      return;
    }
    const samples = 2 * e.data.need;
    const ptr = stream.render(e.data.need);
    let chunk = spare.pop();
    if (!chunk || chunk.length !== samples) chunk = new Float32Array(samples);
    // Copy before the next wasm call, which may memory.grow and detach the view.
    chunk.set(new Float32Array(w.memory.buffer, ptr, samples));
    port.postMessage(chunk, [chunk.buffer]);
  };
}

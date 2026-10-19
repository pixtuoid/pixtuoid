// Audio synthesis OFF the main thread. This worker loads its OWN instance of
// the site's wasm build (memories can't be shared with the page's instance)
// for one of two jobs, named by its first message:
// - the PREWARM: pump the full SynthTake warmup — blocking is fine here — then
//   TRANSFER every buffer to the main thread, one message out, and close so its
//   wasm memory is reclaimed;
// - the MUSIC: own the office's lofi stream for the page's life, rendering each
//   chunk the music worklet asks for over the port it was handed.
'use strict';

// Belt-and-braces vs a non-terminating one-shot pool (mirrors POOL_MAX in
// OfficeBackdrop's discovery loop).
const POOL_MAX = 1024;
// = pool_from_wire's 0-4 domain (audio.rs) — mirrors OfficeBackdrop's list.
const ONESHOT_WIRES = [0, 1, 2, 3, 4];

// The music's stream once built, and the controls that arrived while it was building.
let stream = null;
let asked = null;

self.onmessage = async function (e) {
  if (e.data.controls) {
    if (stream) setControls(e.data.controls);
    else asked = e.data.controls;
    return;
  }
  if (e.data.music) {
    music(e.data.wasmJsUrl, e.data.music);
    return;
  }
  try {
    const mod = await import(e.data.wasmJsUrl);
    const w = await mod.default();
    const take = new mod.SynthTake();
    while (take.step() > 0) {
      /* one piece per step; no main thread to yield to */
    }
    // Copy out of wasm linear memory IMMEDIATELY after each ptr read — a wasm
    // call between view and copy could memory.grow and detach the view.
    const copy = function (ptr, len) {
      return new Float32Array(w.memory.buffer, ptr, len).slice();
    };
    const loops = [];
    const transfers = [];
    // Loops aren't self-terminating like the pools, so the bound can't be
    // discovered — loop_count() reads the ONE authority (LoopStem::ALL).
    const loopCount = take.loop_count();
    for (let i = 0; i < loopCount; i++) {
      const t = copy(take.loop_ptr(i), take.loop_len(i));
      loops.push(t);
      transfers.push(t.buffer);
    }
    const oneshots = {};
    for (const wire of ONESHOT_WIRES) {
      oneshots[wire] = [];
      for (let j = 0; j < POOL_MAX && take.oneshot_len(wire, j) > 0; j++) {
        const t = copy(take.oneshot_ptr(wire, j), take.oneshot_len(wire, j));
        oneshots[wire].push(t);
        transfers.push(t.buffer);
      }
    }
    self.postMessage({ ok: true, loops: loops, oneshots: oneshots }, transfers);
  } catch (err) {
    try {
      self.postMessage({ ok: false });
    } catch (e2) {
      /* channel dead — the main thread's timeout guard settles it */
    }
  }
  self.close();
};

function setControls([daylight, intensity, rain]) {
  stream.set_controls(daylight, intensity, rain);
}

// `start.controls` are the office's first ([daylight, intensity, rain]): they pick the first song, so the stream is
// built with them before it renders. Any message back to the main thread means the music failed.
async function music(wasmJsUrl, start) {
  let w;
  try {
    const mod = await import(wasmJsUrl);
    w = await mod.default();
    const [daylight, intensity, rain] = start.controls;
    stream = new mod.MusicStream(start.nowMs, daylight, intensity, rain);
  } catch (err) {
    self.postMessage({ ok: false });
    self.close();
    return;
  }
  if (asked) setControls(asked);
  const spare = [];
  start.port.onmessage = function (e) {
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
    start.port.postMessage(chunk, [chunk.buffer]);
  };
}

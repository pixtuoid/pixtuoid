// Audio synthesis OFF the main thread. This worker loads its OWN instance of
// the site's wasm build (memories can't be shared with the page's instance),
// pumps the full SynthTake warmup — blocking is fine here — then TRANSFERS
// every buffer to the main thread. One message in, one out, then the worker
// closes so its wasm memory is reclaimed.
'use strict';

// Belt-and-braces vs a non-terminating one-shot pool (mirrors POOL_MAX in
// OfficeBackdrop's discovery loop).
const POOL_MAX = 1024;
// = pool_from_wire's 0-4 domain (audio.rs) — mirrors OfficeBackdrop's list.
const ONESHOT_WIRES = [0, 1, 2, 3, 4];

self.onmessage = async function (e) {
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

// The live offices' host: it owns the office wasm and every `Office` the page opens, and steps and draws one when the
// page asks. office-driver.js's `officeHost` decides where it runs.
'use strict';

import { loadWasm } from './office-driver.js';

// The boot roll: how many floors scroll past before the office settles, how far its colour channels split, its
// ease-out's power, the share of it spent fading in, and how much of a floor divider's office shows.
const FLOORS_PAST = 3.2;
const CHROMA_SPLIT_UNITS = 3;
const ROLL_EASE_POWER = 4;
const ROLL_FADE_IN = 0.15;
const SEAM_ALPHA = 0.3;

// Answer the page's requests on `port` (a worker's own scope, or a page-thread port), one reply per request by `seq`:
// `{ seq, value }`, or `{ seq, error }` when the op threw.
export function serve(port) {
  let mod = null;
  let wasm = null;
  const offices = new Map();

  const ops = {
    async init({ wasmJsUrl }) {
      mod = await loadWasm(wasmJsUrl);
      wasm = await mod.default();
      return [true];
    },
    open({ id, seed, canvas }) {
      const office = new mod.Office(seed);
      offices.set(id, { office, canvas, ctx: canvas.getContext('2d'), img: null });
      return [
        {
          audio: {
            sampleRate: office.audio_sample_rate(),
            ceilingDbfs: office.audio_ceiling_dbfs(),
            attackS: office.audio_limiter_attack_s(),
            releaseS: office.audio_limiter_release_s(),
            musicAheadS: office.audio_music_ahead_s(),
            dezipperS: office.audio_gain_dezipper_s(),
          },
        },
      ];
    },
    // Step to `now` at `w`×`h` layout units, then draw: `draw` null leaves the canvas as it is, 1 blits the frame, and
    // under 1 is the boot roll's progress. `overlay` and `audio` ask for the step's captions and a tick's sound
    // commands, which the page applies or drops as it stands when the reply lands.
    frame({ id, now, w, h, draw, overlay, audio }) {
      const o = offices.get(id);
      o.office.step(now, w, h);
      if (draw !== null) paint(o, w, h, draw);
      return [
        {
          overlay: overlay ? o.office.overlay_json() : null,
          audio: audio ? o.office.audio_tick(now) : null,
        },
      ];
    },
    call({ id, method, args }) {
      return [offices.get(id).office[method](...args)];
    },
    // Synthesize the ♩'s buffers one piece per task, so the frames between pieces still run; with `take`, hand copies
    // over once they're built.
    warmAudio({ id, take }) {
      const office = offices.get(id).office;
      office.audio_begin();
      return new Promise(function (resolve, reject) {
        (function pump() {
          try {
            if (office.audio_warmup_step() > 0) setTimeout(pump, 0);
            else resolve(take ? audioTake(office) : [null]);
          } catch (err) {
            reject(err);
          }
        })();
      });
    },
  };

  // Re-read ptr AND len on every frame: a resize or `memory.grow` reallocates and invalidates a retained view.
  function paint(o, w, h, rt) {
    const scale = o.office.scale();
    const fw = w * scale;
    const fh = h * scale;
    if (o.canvas.width !== fw || o.canvas.height !== fh) {
      o.canvas.width = fw;
      o.canvas.height = fh;
      o.img = null;
    }
    if (!o.img) o.img = new ImageData(fw, fh);
    const view = new Uint8ClampedArray(
      wasm.memory.buffer,
      o.office.frame_ptr(),
      o.office.frame_len()
    );
    if (rt >= 1) o.img.data.set(view);
    else roll(o.img, view, rt, scale);
    o.ctx.putImageData(o.img, 0, 0);
  }

  // Copy each buffer out before the next wasm call, which may `memory.grow` and detach the view.
  function audioTake(office) {
    const copy = function (ptr, len) {
      return new Float32Array(wasm.memory.buffer, ptr, len).slice();
    };
    const loops = [];
    for (let i = 0; i < office.audio_loop_count(); i++) {
      loops.push(copy(office.audio_loop_ptr(i), office.audio_loop_len(i)));
    }
    const oneshots = {};
    for (const wire of office.audio_oneshot_wires()) {
      oneshots[wire] = [];
      // A pool's end is its first empty slot, so its size lives in Rust alone.
      for (let j = 0; office.audio_oneshot_len(wire, j) > 0; j++) {
        oneshots[wire].push(
          copy(office.audio_oneshot_ptr(wire, j), office.audio_oneshot_len(wire, j))
        );
      }
    }
    const buffers = loops.concat(...Object.values(oneshots)).map(function (a) {
      return a.buffer;
    });
    return [{ loops, oneshots }, buffers];
  }

  port.onmessage = function (e) {
    const msg = e.data;
    new Promise(function (resolve) {
      resolve(ops[msg.op](msg));
    }).then(
      function ([value, transfer]) {
        port.postMessage({ seq: msg.seq, value }, transfer || []);
      },
      function (err) {
        port.postMessage({ seq: msg.seq, error: String(err) });
      }
    );
  };
}

// putImageData writes the roll's sub-1 alpha straight to the canvas, so the element's own background shows through.
function roll(img, view, rt, scale) {
  const w = img.width;
  const h = img.height;
  const d = img.data;
  const e = 1 - Math.pow(1 - rt, ROLL_EASE_POWER);
  const shift = Math.round((1 - e) * h * FLOORS_PAST);
  const alpha = Math.min(1, rt / ROLL_FADE_IN);
  const split = Math.round((1 - e) * CHROMA_SPLIT_UNITS * scale);
  for (let y = 0; y < h; y++) {
    const sy = (y + shift) % h;
    const seam = sy < scale || sy >= h - scale ? SEAM_ALPHA : 1;
    const a = alpha * seam * 255;
    const row = sy * w;
    for (let x = 0; x < w; x++) {
      const o = (y * w + x) * 4;
      const xr = split ? (x + split < w ? x + split : w - 1) : x;
      const xb = split ? (x - split >= 0 ? x - split : 0) : x;
      d[o] = view[(row + xr) * 4];
      d[o + 1] = view[(row + x) * 4 + 1];
      d[o + 2] = view[(row + xb) * 4 + 2];
      d[o + 3] = a;
    }
  }
}

// Loaded as a worker (no `window`), it serves its own scope.
if (typeof window === 'undefined') serve(self);

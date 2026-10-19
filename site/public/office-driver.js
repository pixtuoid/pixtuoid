// The shared wasm-`Office` driver for OfficeBackdrop and Showcase's VIBING channel, and OfficeBackdrop's audio bus and
// music.
// Both are `is:inline` scripts (CSP-hashed), which the bundler doesn't process, so
// they can't static-import a shared `src/` module — they dynamic-`import()` this
// public ES module at boot instead.
'use strict';

const WASM_INIT_RETRIES = 2; // extra attempts after the first
const WASM_INIT_BACKOFF_MS = 400;

// The ONE memoized wasm-glue init both live-office consumers share. The generated
// glue's `__wbg_init` memoizes only the RESOLVED instance, not an in-flight promise,
// so two boots racing before either resolves would instantiate two wasm instances
// and stomp the module-global `wasm` — whichever Office built against the loser then
// reads the wrong linear memory forever. The bounded retry MUST stay INSIDE this one
// shared promise: a per-consumer retry would reintroduce that race.
export function sharedWasm(wasmJsUrl) {
  if (window.__pixWasm) return window.__pixWasm;
  function attempt(left) {
    return import(wasmJsUrl)
      .then(function (m) {
        return m.default().then(function () {
          return m;
        });
      })
      .catch(function (err) {
        if (left <= 0) throw err;
        return new Promise(function (res) {
          setTimeout(res, WASM_INIT_BACKOFF_MS);
        }).then(function () {
          return attempt(left - 1);
        });
      });
  }
  window.__pixWasm = attempt(WASM_INIT_RETRIES);
  return window.__pixWasm;
}

// The engine never strikes in a lightning bucket's last STRIKE_GAP_MS
// (pixtuoid-scene's sky; config/scrub-quiet.test.mjs pins both against it).
const LIGHTNING_PERIOD_MS = 15000;
const STRIKE_GAP_MS = 1000;

// The strike-free instant in `t`'s lightning bucket: where a clock jump lands
// so it can't start a flash (WCAG 2.3.1 caps flashes at three a second).
export function quietInstant(t) {
  const bucket = Math.floor(t / LIGHTNING_PERIOD_MS) * LIGHTNING_PERIOD_MS;
  return bucket + LIGHTNING_PERIOD_MS - STRIKE_GAP_MS;
}

// A fresh view over the office's RGBA frame buffer. Re-read ptr AND len on EVERY
// frame — a resize / `memory.grow` reallocates and invalidates any retained view.
export function officeFrameView(office, wasm) {
  return new Uint8ClampedArray(wasm.memory.buffer, office.frame_ptr(), office.frame_len());
}

// W3C Web Audio, § DynamicsCompressorNode Processing: "Compute reduction gain to be compressor gain multiplied by the
// return value of computing the makeup gain", (1 / curve(1.0)) to this power — a lift even under the threshold.
const MAKEUP_EXPONENT = 0.6;

const dbToAmplitude = (db) => 10 ** (db / 20);

// The bus every ♩ lane and one-shot sums into, built once per AudioContext: a limiter at the native sink's ceiling,
// so no sum of them clips. Returns the node they connect to.
export function audioBus(ctx, office) {
  const ceilingDbfs = office.audio_ceiling_dbfs();
  const ceiling = dbToAmplitude(ceilingDbfs);
  // The spec's attack is the time to cut the gain by 10 dB, rodio's a one-pole time constant: one value sets both
  // limiters' speed, not their curve.
  const limiter = new DynamicsCompressorNode(ctx, {
    threshold: ceilingDbfs,
    knee: 0,
    attack: office.audio_limiter_attack_s(),
    release: office.audio_limiter_release_s(),
  });
  limiter.ratio.value = limiter.ratio.maxValue;
  // curve(1.0) in dB, on the ratio segment a zero knee leaves above the threshold.
  const fullScaleDb = limiter.threshold.value * (1 - 1 / limiter.ratio.value);
  const makeup = (1 / dbToAmplitude(fullScaleDb)) ** MAKEUP_EXPONENT;
  // § WaveShaperNode: "Any sample value greater than +1 will correspond to the last value in the curve array", and
  // less than -1 to the first, so the curve [-ceiling, ceiling] over the signal divided by the ceiling clamps it there.
  const trim = new GainNode(ctx, { gain: 1 / makeup / ceiling });
  const clamp = new WaveShaperNode(ctx, { curve: new Float32Array([-ceiling, ceiling]) });
  limiter.connect(trim).connect(clamp).connect(ctx.destination);
  return limiter;
}

// The ♩ music: the office's lofi stream, rendered in the music worker (`urls.worker`, loading `urls.wasm`) and played
// by the worklet (`urls.worklet`) `aheadS` ahead, the two joined by their own MessageChannel so the main thread stays
// off the audio path. `controls`, a tick's `music.controls`, pick the first song. Resolves, once the worklet loads, to
// `{ node, primed, failed, setControls, stop }`: `node` to connect, `primed` on the worklet's first chunk, `failed` on
// the worker's death; rejects where there is no AudioWorklet.
export async function startMusic(ctx, urls, aheadS, nowMs, controls) {
  await ctx.audioWorklet.addModule(urls.worklet);
  const node = new AudioWorkletNode(ctx, 'pix-music', {
    numberOfInputs: 0,
    outputChannelCount: [2],
    processorOptions: { aheadS },
  });
  const worker = new Worker(urls.worker, { type: 'module' });
  const failed = new Promise(function (resolve) {
    worker.onerror = resolve;
    worker.onmessage = resolve;
  });
  const primed = new Promise(function (resolve) {
    node.port.onmessage = function (e) {
      if (e.data.primed) resolve();
    };
  });
  const channel = new MessageChannel();
  worker.postMessage({ wasmJsUrl: urls.wasm, port: channel.port1, nowMs, controls }, [
    channel.port1,
  ]);
  node.port.postMessage({ source: channel.port2 }, [channel.port2]);
  return {
    node,
    primed,
    failed,
    setControls(c) {
      worker.postMessage({ controls: c });
    },
    stop() {
      worker.terminate();
      node.disconnect();
    },
  };
}

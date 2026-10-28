// The shared live-office driver for OfficeBackdrop and Showcase's VIBING channel, and OfficeBackdrop's audio bus and
// music. Both are `is:inline` scripts (CSP-hashed), which the bundler doesn't process, so they can't static-import a
// shared `src/` module — they dynamic-`import()` this public ES module at boot instead. office-host.js and
// music-worker.js static-import `loadWasm` into module workers, which have no `window`: module scope touches no page
// global.
'use strict';

const WASM_INIT_RETRIES = 2; // extra attempts after the first
const WASM_INIT_BACKOFF_MS = 400;

// The page's ONE office host (office-host.js), which both live-office consumers open their `Office` on, so one wasm
// instance serves the page. A module worker runs it wherever a canvas can hand its drawing to one, so the wasm's boot
// and frames never block the page's thread (Lighthouse's TBT); else the page's thread does. A host whose wasm never
// loaded clears the memo, so a later consumer starts a fresh one (#671). Resolves to `{ offThread, open(seed, canvas) }`;
// `open` resolves to the office's handle, whose `frame`, `call` and `warmAudio` are office-host.js's ops of those names.
export function officeHost(hostUrl, wasmJsUrl) {
  if (!window.__pixOfficeHost) {
    const host = startHost(hostUrl, wasmJsUrl);
    window.__pixOfficeHost = host;
    host.catch(function () {
      if (window.__pixOfficeHost === host) window.__pixOfficeHost = null;
    });
  }
  return window.__pixOfficeHost;
}

async function startHost(hostUrl, wasmJsUrl) {
  let worker = null;
  if ('transferControlToOffscreen' in HTMLCanvasElement.prototype) {
    try {
      worker = new Worker(hostUrl, { type: 'module' });
    } catch (err) {
      // a browser without module workers keeps the host on this thread
    }
  }
  const port = worker || (await pageThreadPort(hostUrl));
  const pending = new Map();
  let seq = 0;
  let ids = 0;
  port.onmessage = function (e) {
    const p = pending.get(e.data.seq);
    if (!p) return; // a reply the worker's error already rejected
    pending.delete(e.data.seq);
    if ('error' in e.data) p.reject(new Error(e.data.error));
    else p.resolve(e.data.value);
  };
  if (worker) {
    worker.onerror = function (e) {
      pending.forEach(function (p) {
        p.reject(e);
      });
      pending.clear();
    };
  }
  function rpc(op, args, transfer) {
    return new Promise(function (resolve, reject) {
      seq += 1;
      pending.set(seq, { resolve, reject });
      port.postMessage(Object.assign({ seq, op }, args), transfer || []);
    });
  }
  try {
    await rpc('init', { wasmJsUrl });
  } catch (err) {
    if (worker) worker.terminate();
    throw err;
  }
  return {
    offThread: !!worker,
    open(seed, canvas) {
      const id = ++ids;
      const target = worker ? canvas.transferControlToOffscreen() : canvas;
      return rpc('open', { id, seed, canvas: target }, worker ? [target] : []).then(
        function (info) {
          let busy = false;
          return {
            audio: info.audio,
            // A frame is in the host: its caller holds the next one rather than queue frames behind it.
            get busy() {
              return busy;
            },
            frame(req) {
              busy = true;
              return rpc('frame', Object.assign({ id }, req)).finally(function () {
                busy = false;
              });
            },
            call(method, ...args) {
              return rpc('call', { id, method, args });
            },
            warmAudio(take) {
              return rpc('warmAudio', { id, take });
            },
          };
        }
      );
    },
  };
}

// The host on this thread, behind a port pair that hands each message over a task later, in order, as a worker's
// would — by reference, since a canvas element can't be cloned.
async function pageThreadPort(hostUrl) {
  const { serve } = await import(hostUrl);
  const page = {};
  const host = {};
  page.postMessage = function (data) {
    setTimeout(function () {
      host.onmessage({ data });
    }, 0);
  };
  host.postMessage = function (data) {
    setTimeout(function () {
      page.onmessage({ data });
    }, 0);
  };
  serve(host);
  return page;
}

// A wasm glue module once its wasm instantiates, retried a bounded number of times: one dropped fetch must not
// strand its office or its music until a reload (#671).
export function loadWasm(wasmJsUrl) {
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
  return attempt(WASM_INIT_RETRIES);
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

// W3C Web Audio, § DynamicsCompressorNode Processing: "Compute reduction gain to be compressor gain multiplied by the
// return value of computing the makeup gain", (1 / curve(1.0)) to this power — a lift even under the threshold.
const MAKEUP_EXPONENT = 0.6;

const dbToAmplitude = (db) => 10 ** (db / 20);

// The bus every ♩ lane and one-shot sums into, built once per AudioContext: a limiter at the native sink's ceiling,
// so no sum of them clips. `audio` is an office handle's constants. Returns the node they connect to.
export function audioBus(ctx, audio) {
  const ceilingDbfs = audio.ceilingDbfs;
  const ceiling = dbToAmplitude(ceilingDbfs);
  // The spec's attack is the time to cut the gain by 10 dB, rodio's a one-pole time constant: one value sets both
  // limiters' speed, not their curve.
  const limiter = new DynamicsCompressorNode(ctx, {
    threshold: ceilingDbfs,
    knee: 0,
    attack: audio.attackS,
    release: audio.releaseS,
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

// The ♩ music: the office's lofi stream, rendered in the music worker (`urls.worker`, loading the music's own wasm
// glue, `urls.musicWasm`) and played
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
  worker.postMessage({ musicWasmJsUrl: urls.musicWasm, port: channel.port1, nowMs, controls }, [
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

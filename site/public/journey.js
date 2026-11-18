// The How it works page's journey: each click sends the office wasm's next recorded hook line through
// `Office.send_hook`, the real decoder and reducer, and shows what each of the figure's stops
// (rehype-figures.mjs's JOURNEY_STOPS) made of it, the stop `frame`'s output being the player's office.
'use strict';

import { officeHost, wholeBox } from './office-driver.js';

// Its own office's seed, apart from the home page's.
const SEED = 7;
// How long a stop stays the newest lit before the next one lights.
const STEP_MS = 280;
// How long the office moves after it boots, resizes or takes a line, then holds still: within WCAG 2.2.2's five
// seconds, past which motion that starts by itself needs a pause control
// (w3.org/WAI/WCAG22/Understanding/pause-stop-hide).
export const MOVING_MS = 5000;

const reduced = matchMedia('(prefers-reduced-motion: reduce)');

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

// What a line is, in its agent's own words: the hook's name, and the tool it runs.
function hookName(payload) {
  return [payload.hook_event_name, payload.tool_name].filter(Boolean).join(' · ');
}

// `payload` as JSON, the keys in `marked` drawn as the shim's.
function jsonInto(pre, payload, marked) {
  pre.replaceChildren();
  const keys = Object.keys(payload);
  pre.append('{\n');
  keys.forEach(function (key, i) {
    const line = '  ' + JSON.stringify(key) + ': ' + JSON.stringify(payload[key]);
    const comma = i < keys.length - 1 ? ',\n' : '\n';
    if (marked.includes(key)) pre.append(el('mark', 'fig-journey__stamp', line), comma);
    else pre.append(line + comma);
  });
  pre.append('}');
}

export async function start(figure) {
  const stops = {};
  figure.querySelectorAll('[data-stop]').forEach(function (li) {
    stops[li.dataset.stop] = li;
  });
  const player = el('div', 'fig-journey__player');
  // The canvas is sized to whole CSS pixels, a little past its crop box, which clips it.
  const crop = el('div', 'fig-journey__crop');
  const canvas = el('canvas', 'fig-journey__office');
  canvas.setAttribute('role', 'img');
  canvas.setAttribute('aria-label', 'The office, drawn by the engine');
  crop.append(canvas);
  player.append(crop);
  figure.prepend(player);
  // The player sticks under the nav only while it takes at most half the viewport below it, so a short
  // screen keeps room for the stops it fills.
  function stickiness() {
    const nav = parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--nav-h'));
    player.classList.toggle('is-sticky', player.offsetHeight <= (window.innerHeight - nav) / 2);
  }
  new ResizeObserver(stickiness).observe(player);
  window.addEventListener('resize', stickiness);
  // A stop focused from the keyboard under the stuck player scrolls clear of it (WCAG 2.4.11): the
  // browser's own focus scroll stops at the viewport's edge, which the player covers. A click focuses
  // what the reader already sees, and scrolling it would pull the text from under the pointer.
  figure.addEventListener('focusin', function (e) {
    if (player.contains(e.target) || !e.target.matches(':focus-visible')) return;
    const under = player.getBoundingClientRect().bottom - e.target.getBoundingClientRect().top;
    if (under > 0) window.scrollBy({ top: -under });
  });
  const outs = {};
  try {
    await run();
  } catch (err) {
    // Whatever the page added goes, so the stops stay the prose they were.
    player.remove();
    Object.values(outs).forEach(function (pre) {
      pre.remove();
    });
    throw err;
  }

  async function run() {
    const host = await officeHost();
    const office = await host.open(SEED, canvas, true);
    const lines = office.journey.lines;
    const payloads = lines.map(function (line) {
      return JSON.parse(line);
    });
    ['agent', 'shim', 'decoder', 'office'].forEach(function (role) {
      const pre = el('pre', 'fig-journey__out');
      // A scroll container takes focus, so a keyboard can scroll it (WCAG 2.1.1).
      pre.tabIndex = 0;
      pre.hidden = true;
      stops[role].querySelector('.fig-journey__what').after(pre);
      outs[role] = pre;
    });

    // The office's clock: under reduced motion it stands where it stood when motion was last reduced,
    // else it is the wall clock; each adds every jump a line took (the office's `jumpMs`, under reduced
    // motion), so it never runs backward.
    let ahead = 0;
    let stood = Date.now();
    reduced.addEventListener('change', function () {
      stood = Date.now();
    });
    const clock = function () {
      return (reduced.matches ? stood : Date.now()) + ahead;
    };
    let units = [0, 0];
    let fitSeq = 0;
    async function fit() {
      const dpr = window.devicePixelRatio || 1;
      const r = crop.getBoundingClientRect();
      // A reply that lands after a later fit was asked sizes nothing.
      const seq = ++fitSeq;
      const reply = await office.call('fit', Math.round(r.width * dpr), Math.round(r.height * dpr));
      if (seq !== fitSeq) return;
      const box = wholeBox(reply, dpr);
      units = box.units;
      canvas.style.width = box.css[0] + 'px';
      canvas.style.height = box.css[1] + 'px';
      canvas.dataset.upscale = String(reply[3]);
    }
    // Nothing to draw until a fit has landed: the resize observer's first fit supersedes the boot's.
    function draw() {
      if (office.busy || !units[0]) return Promise.resolve();
      return office.frame({
        now: clock(),
        w: units[0],
        h: units[1],
        draw: 1,
        overlay: false,
        audio: false,
      });
    }
    let raf = 0;
    let inView = false;
    let movingUntil = 0;
    function loop() {
      raf = 0;
      // Under reduced motion the frames repeat one instant, which also lets the engine's flash hold
      // pass the frame it held.
      if (!inView || document.hidden || Date.now() > movingUntil) return;
      draw().catch(function () {
        /* a host that died leaves the last frame up */
      });
      raf = requestAnimationFrame(loop);
    }
    function sync() {
      if (!raf) loop();
    }
    function move() {
      movingUntil = Date.now() + MOVING_MS;
      sync();
    }
    new IntersectionObserver(function (entries) {
      inView = entries[0].isIntersecting;
      sync();
    }).observe(canvas);
    document.addEventListener('visibilitychange', sync);
    new ResizeObserver(function () {
      fit().then(move);
    }).observe(crop);
    await fit();
    await draw();
    move();

    const send = el('div', 'fig-journey__send');
    send.setAttribute('role', 'group');
    send.setAttribute('aria-label', 'Send the recorded hooks, in order');
    const status = el('p', 'sr-only');
    status.setAttribute('aria-live', 'polite');
    let next = 0;
    let landing = false;
    const buttons = payloads.map(function (payload, i) {
      const button = el('button', 'fig-journey__hook');
      button.type = 'button';
      button.append(el('span', 'fig-journey__n', String(i + 1)), ' ', hookName(payload));
      button.addEventListener('click', function () {
        land(i, document.activeElement === button);
      });
      return button;
    });
    function syncButtons() {
      buttons.forEach(function (button, i) {
        button.disabled = landing || i !== next;
        button.classList.toggle('is-sent', i < next);
      });
    }
    send.append(...buttons);
    player.append(send, status);
    syncButtons();

    const wait = function (ms) {
      return new Promise(function (resolve) {
        setTimeout(resolve, reduced.matches ? 0 : ms);
      });
    };
    // `focused`: the send came from the focused button, whose disabling drops focus to the body, so focus
    // goes on to the button that sends next (the same one, to retry a failure) unless the reader moved it.
    async function land(i, focused) {
      landing = true;
      syncButtons();
      try {
        await show(i);
        next = (i + 1) % lines.length;
      } finally {
        landing = false;
        syncButtons();
        if (focused && document.activeElement === document.body) buttons[next].focus();
      }
    }
    async function show(i) {
      const landed = JSON.parse(await office.call('send_hook', lines[i], clock()));
      const agent = Object.fromEntries(
        Object.entries(payloads[i]).filter(function ([key]) {
          return !landed.stamps.includes(key);
        })
      );
      const fills = [
        ['agent', () => jsonInto(outs.agent, agent, [])],
        ['shim', () => jsonInto(outs.shim, payloads[i], landed.stamps)],
        ['decoder', () => (outs.decoder.textContent = JSON.stringify(landed.events, null, 2))],
        ['office', () => (outs.office.textContent = JSON.stringify(landed.agents, null, 2))],
        [
          'frame',
          () => {
            if (reduced.matches) ahead += office.journey.jumpMs;
            move();
          },
        ],
      ];
      Object.values(stops).forEach(function (li) {
        li.classList.remove('is-lit');
      });
      for (const [role, fill] of fills) {
        await fill();
        if (outs[role]) outs[role].hidden = false;
        stops[role].classList.add('is-lit');
        await wait(STEP_MS);
      }
      const n = landed.events.length;
      status.textContent = `${hookName(payloads[i])} sent: the decoder made ${n} ${n === 1 ? 'event' : 'events'}.`;
    }
  }
}

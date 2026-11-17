// The How it works page's journey: each click sends the office wasm's next recorded hook line through
// `Office.send_hook`, the real decoder and reducer, and shows what each of the figure's stops
// (rehype-figures.mjs's JOURNEY_STOPS) made of it. The player above the stops draws the office, the stop
// `frame`'s output, where it stays in view while the stops below are read.
'use strict';

// Its own office's seed, apart from the home page's.
const SEED = 7;
// How long a stop stays the newest lit before the next one lights.
const STEP_MS = 280;
// Under reduced motion the canvas draws one still per line, this far on, where a walk has ended.
const SETTLED_MS = 4000;

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

// The slot's fields the office shows, by the reducer's own names.
function slotView(slot) {
  return {
    label: slot.label.text,
    desk: slot.desk,
    state: slot.state,
    tool_call_count: slot.tool_call_count,
    exiting_at: slot.exiting_at,
  };
}

export async function start(figure, urls) {
  const stops = {};
  figure.querySelectorAll('[data-stop]').forEach(function (li) {
    stops[li.dataset.stop] = li;
  });
  const player = el('div', 'fig-journey__player');
  const canvas = el('canvas', 'fig-journey__office');
  canvas.setAttribute('role', 'img');
  canvas.setAttribute('aria-label', 'The office, drawn by the engine');
  player.append(canvas);
  figure.prepend(player);
  let office;
  try {
    const drv = await import(urls.driver);
    const host = await drv.officeHost(urls.host, urls.wasm);
    office = await host.open(SEED, canvas, true);
  } catch (err) {
    // Without the office the stops stay the prose they were.
    player.remove();
    throw err;
  }
  const outs = {};
  ['agent', 'shim', 'decoder', 'office'].forEach(function (role) {
    const pre = el('pre', 'fig-journey__out');
    // A scroll container takes focus, so a keyboard can scroll it (WCAG 2.1.1).
    pre.tabIndex = 0;
    pre.hidden = true;
    stops[role].querySelector('.fig-journey__what').after(pre);
    outs[role] = pre;
  });
  const lines = office.journeyLines;
  const payloads = lines.map(function (line) {
    return JSON.parse(line);
  });

  let units = [0, 0];
  async function fit() {
    const dpr = window.devicePixelRatio || 1;
    const [w, h] = await office.call(
      'fit',
      Math.round(canvas.clientWidth * dpr),
      Math.round(canvas.clientHeight * dpr)
    );
    units = [w, h];
  }
  function draw(now) {
    if (office.busy) return Promise.resolve();
    return office.frame({ now, w: units[0], h: units[1], draw: 1, overlay: false, audio: false });
  }

  let raf = 0;
  let inView = false;
  function loop() {
    raf = 0;
    if (!inView || document.hidden || reduced.matches) return;
    draw(Date.now()).catch(function () {
      /* a host that died leaves the last frame up */
    });
    raf = requestAnimationFrame(loop);
  }
  function sync() {
    if (!raf) loop();
  }
  new IntersectionObserver(function (entries) {
    inView = entries[0].isIntersecting;
    sync();
  }).observe(canvas);
  document.addEventListener('visibilitychange', sync);
  reduced.addEventListener('change', sync);
  new ResizeObserver(function () {
    fit().then(function () {
      if (reduced.matches) draw(Date.now() + SETTLED_MS);
    });
  }).observe(canvas);
  await fit();
  await draw(Date.now());

  const send = el('div', 'fig-journey__send');
  send.setAttribute('role', 'group');
  send.setAttribute('aria-label', 'Send the recorded hooks, in order');
  const status = el('p', 'sr-only');
  status.setAttribute('aria-live', 'polite');
  let next = 0;
  let busy = false;
  const buttons = payloads.map(function (payload, i) {
    const button = el('button', 'fig-journey__hook');
    button.type = 'button';
    button.append(el('span', 'fig-journey__n', String(i + 1)), ' ', hookName(payload));
    button.addEventListener('click', function () {
      land(i);
    });
    return button;
  });
  function syncButtons() {
    buttons.forEach(function (button, i) {
      button.disabled = busy || i !== next;
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
  async function land(i) {
    busy = true;
    syncButtons();
    try {
      await show(i);
      next = (i + 1) % lines.length;
    } finally {
      busy = false;
      syncButtons();
    }
  }
  async function show(i) {
    const now = Date.now();
    const landed = JSON.parse(await office.call('send_hook', lines[i], now));
    const agent = Object.fromEntries(
      Object.entries(payloads[i]).filter(function ([key]) {
        return !landed.stamps.includes(key);
      })
    );
    const shown = [
      ['agent', () => jsonInto(outs.agent, agent, [])],
      ['shim', () => jsonInto(outs.shim, payloads[i], landed.stamps)],
      ['decoder', () => (outs.decoder.textContent = JSON.stringify(landed.events, null, 2))],
      [
        'office',
        () => (outs.office.textContent = JSON.stringify(landed.agents.map(slotView), null, 2)),
      ],
      ['frame', () => reduced.matches && draw(now + SETTLED_MS)],
    ];
    Object.values(stops).forEach(function (li) {
      li.classList.remove('is-lit');
    });
    for (const [role, show] of shown) {
      show();
      if (outs[role]) outs[role].hidden = false;
      stops[role].classList.add('is-lit');
      await wait(STEP_MS);
    }
    const n = landed.events.length;
    status.textContent = `${hookName(payloads[i])} sent: the decoder made ${n} ${n === 1 ? 'event' : 'events'}.`;
  }
}

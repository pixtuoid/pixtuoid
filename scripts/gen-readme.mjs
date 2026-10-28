#!/usr/bin/env node
// Regenerate the README's marker-delimited blocks from the site's single-source
// JSON (features.json, sources.json, install.json) — the same files
// Showcase/SupportedTools/Install read, so README and site can't drift — and
// its captioned images from scripts/media.json's jobs.
// `--check` writes nothing and exits non-zero on drift.
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import process from 'node:process';
import features from '../site/src/features.json' with { type: 'json' };
import sources from '../site/src/sources.json' with { type: 'json' };
import install from '../site/src/install.json' with { type: 'json' };
import media from './media.json' with { type: 'json' };
import { featureText } from '../site/config/feature-text.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const readmePath = join(root, 'README.md');

// MUST match `site` in site/astro.config.mjs — a hand-kept copy (importing the
// astro config here would pull @astrojs/*), and nothing gates the two.
const SITE = 'https://pixtuoid.dev';
const check = process.argv.includes('--check');
let readme = readFileSync(readmePath, 'utf8');
const errors = [];

// Every feature `pix` must resolve to a committed pixel-icon SVG. PixIcon.astro
// throws at build ONLY for roster-rendered icons; a channel-bearing feature
// reaches this README `<img>` but never PixIcon, so a typo'd `pix` would ship a
// 404 image past every other check.
for (const f of features) {
  if (f.pix && !existsSync(join(root, 'docs', 'images', 'pix-icons', `${f.pix}.svg`))) {
    errors.push(
      `feature "${f.name}" declares pix "${f.pix}" but docs/images/pix-icons/${f.pix}.svg is missing — ` +
        `add "${f.pix}" to gen-pix-icons.py's ICONS and run \`just gen-icons\`.`
    );
  }
}

// Neutralize only what breaks a GFM table row. `|` uses the HTML entity —
// backslash-escaping would itself need backslash escaping first (CodeQL
// js/incomplete-sanitization). Cell text is intentionally markdown-bearing.
const cell = (/** @type {unknown} */ s) => String(s).replace(/\|/g, '&#124;').replace(/\r?\n/g, ' ');

// The `() => block` replacer must stay a FUNCTION: it inserts the value
// literally, where a plain string would expand `$`-patterns ($$, $&, $') in the
// text and corrupt the README in a way --check can't see (both sides of its
// comparison would go through the same mangling).
/**
 * @param {string} label
 * @param {string} start
 * @param {string} end
 * @param {string} body
 */
function regenSection(label, start, end, body) {
  const block = `${start}\n${body}\n${end}`;
  const re = new RegExp(`${escapeRe(start)}[\\s\\S]*?${escapeRe(end)}`);
  if (!re.test(readme)) {
    console.error(`gen-readme: ${label} markers not found in README.md. Expected:\n\n${block}\n`);
    process.exit(1);
  }
  const next = readme.replace(re, () => block);
  if (next === readme) {
    console.log(`README ${label} already up to date ✓`);
    return;
  }
  if (check) {
    errors.push(`README ${label} is stale — run \`just gen-readme\` after editing the JSON.`);
  } else {
    readme = next;
    writeFileSync(readmePath, readme);
    console.log(`✓ README ${label} regenerated`);
  }
}

// A feature is README-featured by DEFAULT — opt a secondary one OUT with
// `"featured": false` (the inverse of install.json's opt-IN `readme:true`).
// GitHub gives the empty-header icon column almost no width and forces
// `max-width:100%` on the <img>, so without explicit dimensions the icon
// collapses to an illegible blob; pin width/height from the SVG's own.
// Returns null if the SVG is missing — the existsSync guard above already
// reports that with an actionable message, so don't pre-empt it with an ENOENT.
const svgWH = (/** @type {string} */ pix) => {
  const p = join(root, 'docs', 'images', 'pix-icons', `${pix}.svg`);
  if (!existsSync(p)) return null;
  const m = readFileSync(p, 'utf8').match(/<svg[^>]* width="(\d+)" height="(\d+)"/);
  return m && [Number(m[1]), Number(m[2])];
};
const pixDims = (/** @type {string} */ pix) => {
  const wh = svgWH(pix);
  return wh ? ` width="${wh[0]}" height="${wh[1]}"` : '';
};
/** @param {(typeof features)[number]} f */
const iconCell = (f) =>
  f.pix ? `<img src="docs/images/pix-icons/${cell(f.pix)}.svg" alt=""${pixDims(f.pix)}>` : cell(f.icon);
const featuredFeatures = features.filter((f) => f.featured !== false);
const featureRows = featuredFeatures.map(
  (f) => `| ${iconCell(f)} | **${cell(f.name)}** | ${cell(featureText(f.desc, f))} |`
);
// GitHub ignores an <img>'s width/height when its table cell is "shorter" than
// the image and collapses the column — hard in Safari, where the injected
// `max-width:100%` makes the img's min-content 0. The GFM fix is
// non-breaking-space "glue": real, text-measured content the collapse can't
// undo. Pad the icon HEADER only, so it doesn't inflate each row's max-content.
const NBSP_PX = 4; // a README-font &nbsp; ≈ 4px
const maxIconW = Math.max(...featuredFeatures.map((f) => (f.pix ? (svgWH(f.pix)?.[0] ?? 0) : 0)));
const iconHeader = '&nbsp;'.repeat(Math.ceil(maxIconW / NBSP_PX) + 2);
regenSection(
  'Features table',
  '<!-- features:start · generated from site/src/features.json by `just gen-readme` — edit the JSON, not this table -->',
  '<!-- features:end -->',
  [`| ${iconHeader} | Feature | Description |`, '|---|---|---|', ...featureRows].join('\n')
);

const supported = sources.filter((s) => s.status === 'supported');
const planned = sources.filter((s) => s.status === 'planned');
const esc = (/** @type {unknown} */ s) => String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
// Two per row: a third column squeezes the longer names on a phone-width page.
const TOOL_COLUMNS = 2;
const toolRows = [];
for (let i = 0; i < supported.length; i += TOOL_COLUMNS) {
  const cells = supported
    .slice(i, i + TOOL_COLUMNS)
    .map((s) => `<td width="${100 / TOOL_COLUMNS}%"><a href="${esc(s.url)}">${esc(s.name)}</a></td>`);
  toolRows.push(`<tr>${cells.join('')}</tr>`);
}
// The grid names no OS, so an OS any supported tool runs on only experimentally
// gets its caveat here: a Windows user installs from this page.
const OS_LABELS = /** @type {const} */ ([
  ['macos', 'macOS'],
  ['linux', 'Linux'],
  ['windows', 'Windows'],
]);
const experimentalNotes = OS_LABELS.flatMap(([os, label]) => {
  const n = supported.filter((s) => s.platforms?.[os] === 'experimental').length;
  if (!n) return [];
  const scope = n === supported.length ? '' : ' for some tools';
  return [`_${label} support is experimental${scope} — limited testing, unsigned binaries._`];
});
regenSection(
  'Supported-tools glimpse',
  '<!-- tools:start · generated from site/src/sources.json by `just gen-readme` — edit the JSON, not this table -->',
  '<!-- tools:end -->',
  [
    '<table>',
    ...toolRows,
    '</table>',
    '',
    ...(planned.length ? [`_Planned: ${planned.map((s) => cell(s.name)).join(', ')}._`, ''] : []),
    `**→ [Every tool × OS on the site](${SITE}/#tools)**`,
    ...experimentalNotes.map((n) => `\n${n}`),
  ].join('\n')
);

// Half the docs jobs' render width, so a 2x screen draws each pixel.
const README_IMAGE_WIDTH = 768;
// A docs image with a `caption`, the caption under it and an `href` (a site
// path) around it. Its file must be committed: media-regen.yml renders
// docs/images from main, so a new image's README PR follows the bot PR that
// commits it.
const codeSpans = (/** @type {string} */ s) => esc(s).replace(/`([^`]+)`/g, '<code>$1</code>');
const captioned = media.filter((j) => 'caption' in j && j.targets.includes('docs'));
for (const job of captioned) {
  const file = `docs/images/${job.id}.${job.kind === 'gif' ? 'gif' : 'png'}`;
  if (!existsSync(join(root, file))) {
    errors.push(`media.json "${job.id}" has a caption but ${file} isn't committed yet — wait for media-regen.yml.`);
    continue;
  }
  // A linked image names where it goes
  // (https://www.w3.org/WAI/tutorials/images/functional/, Example 1); an
  // unlinked one takes `alt=""`, its caption already describing it
  // (https://www.w3.org/WAI/tutorials/images/decorative/, Example 3).
  const href = 'href' in job ? new URL(String(job.href), SITE) : null;
  const alt = href ? `${href.host}${href.pathname}`.replace(/\/$/, '') : '';
  const img = `<img src="${file}" alt="${esc(alt)}" width="${README_IMAGE_WIDTH}" />`;
  const linked = href ? `<a href="${esc(href)}">${img}</a>` : img;
  regenSection(
    `${job.id} image`,
    `<!-- media:${job.id}:start · generated from scripts/media.json by \`just gen-readme\` — edit the JSON, not this block -->`,
    `<!-- media:${job.id}:end -->`,
    `<p align="center">${linked}<br /><sub>${codeSpans(String(job.caption))}</sub></p>`
  );
}
// A block no captioned job renders would go stale unseen.
for (const [, id] of readme.matchAll(/<!-- media:(\S+):start/g)) {
  if (!captioned.some((j) => j.id === id)) {
    errors.push(`README.md has a media:${id} block but scripts/media.json has no captioned docs job "${id}".`);
  }
}

const installBody = install
  .filter((m) => m.readme)
  .map(
    (m) =>
      `**${cell(m.label)}**${m.blurb ? ` (${cell(m.blurb)})` : ''}:\n\n\`\`\`bash\n${m.cmds.join('\n')}\n\`\`\``
  )
  .join('\n\n');
regenSection(
  'Install block',
  '<!-- install:start · generated from site/src/install.json by `just gen-readme` — edit the JSON, not this block -->',
  '<!-- install:end -->',
  installBody
);

if (errors.length) {
  console.error(errors.map((e) => `✗ ${e}`).join('\n'));
  process.exit(1);
}
console.log(
  check
    ? 'README is in sync with features.json + sources.json + install.json + media.json ✓'
    : 'README regenerated from features.json + sources.json + install.json + media.json ✓'
);

/** @param {string} s */
function escapeRe(s) {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

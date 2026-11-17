import themesData from './themes.json';
import weatherData from './weather.json';
import showcaseData from './showcase.json';
import featuresData from './features.json';
import docsData from './docs.json';
import { featureText } from '../config/feature-text.mjs';
import { withBase } from '../config/base-path.mjs';
import { floorById } from '../config/floors.mjs';
import type { VALID_THEMES } from '../config/page-themes.mjs';

export const CRATES = 'https://crates.io/crates/pixtuoid';
export const SPONSOR = 'https://buymeacoffee.com/IvanWng97';

// Every internal URL joins astro.config.mjs's `base` through withBase(), as
// asset() in components and by rehype-repo-links in rendered markdown, so a
// change of `base` can never silently break links.
const BASE = import.meta.env.BASE_URL;
export const asset = (p: string): string => withBase(BASE, p);
// A floor's anchor on the home page, from any page.
export const floorHref = (id: string): string => `${asset('')}#${floorById(id).id}`;

// Seeded parse-first into window.__pixTheme (Base.astro head) via define:vars.
// THEME_BG is the mobile browser-chrome tint; its hexes MIRROR global.css's
// per-theme --bg, a CSS↔JS pairing that can't share a literal — retune both
// together (config/theme-bg.test.mjs asserts they agree).
export const THEME_STORAGE_KEY = 'pix-theme';
export type ThemeId = (typeof VALID_THEMES)[number];
export const THEME_BG: Record<ThemeId, string> = {
  day: '#f4eee2',
  night: '#1d1813',
  dracula: '#282a36',
};

// The ONE floor-spy IntersectionObserver band: the Statusline scrollspy and the
// ElevatorShaft LED each build their own observer over the SAME [data-floor]
// sections, so a one-sided retune would make the two readouts silently disagree.
export const FLOOR_SPY_ROOT_MARGIN = '-45% 0px -45% 0px';
// Rounding slack for the bottom-clamp both floor-spy consumers apply at true
// scroll max (scrollY+innerHeight vs scrollHeight can differ by a subpixel).
export const BOTTOM_CLAMP_EPSILON_PX = 2;

// The dimmer's resting opacity, straddling a JS↔CSS boundary: OfficeBackdrop
// emits it into #dimmer's CSS as `--dim-rest`, Statusline derives its
// 'lights N%' readout from it (100·(1 − DIM_RESTING)).
export const DIM_RESTING = 0.55;

// A page of a wing: one of the reading rooms Wing.astro's shell frames.
export interface WingPage {
  route: string; // base-path-relative, slashed as its page's canonical URL (asset(route))
  label: string;
}
export interface Wing {
  name: string; // its landmarks' accessible name
  bank: string; // the sidebar's elevator bank
  path: string; // the statusline's path
  pages: readonly WingPage[]; // the sidebar's list, and the sequence the pager turns through
  turns: { prev: string; next: string }; // what the pager calls a step up that list, and down it
}
// The rendered docs, in reading order: the Nav dropdown's order too.
export interface DocPage extends WingPage {
  file: string; // under the repo's DOC_DIR (config/docs.mjs), the page's single source
  description: string;
  sourceNote?: string; // what the page's source line adds about how it renders the file
}
export const DOCS: readonly DocPage[] = docsData;
export const DOCS_WING: Wing = {
  name: 'Docs',
  bank: 'docs wing',
  path: 'docs',
  pages: DOCS,
  turns: { prev: 'previous', next: 'next' },
};
export const pageTitle = (name: string): string => `${name} — pixtuoid`;

export interface ThemeShot {
  id: string;
  name: string;
  accent: string;
  accent2: string; // gradient end hue
  featured?: boolean; // shown first in the switcher
}

// A chip's `data-theme` is guaranteed to resolve in-canvas by the Rust-side
// `theme_gallery_manifest_matches_all_themes` set-equality test.
export const THEMES: ThemeShot[] = themesData as ThemeShot[];

interface WeatherShot {
  id: string; // matches `--weather <id>` (the live chip's data-weather)
  name: string;
}

// A chip's `data-weather` is guaranteed to resolve in-canvas by the Rust-side
// `weather_gallery_manifest_matches_the_weather_enum` set-equality test.
const WEATHERS: WeatherShot[] = weatherData as WeatherShot[];

export interface ShowcaseVariant {
  id: string;
  name: string;
  src: string; // public/demos/-relative filename
  accent?: string;
  accent2?: string;
  featured?: boolean; // default chip for its channel
}

// Weather chips preview only; theme chips also retint the page — hence
// per-group `retint` rather than one channel-level flag.
export interface VariantGroup {
  key: 'weather' | 'theme';
  label: string;
  variantsRef: 'themes' | 'weather';
  retint?: boolean;
}

export interface ShowcaseChannel {
  id: string; // slug; hash target #showcase-<id> — ALSO the features.json row's `channel` value
  label: string; // monitor label (channel number is derived from manifest order)
  kind: 'clip' | 'variant-set' | 'live';
  asset?: string; // clip: demos/<asset>.mp4 [+ .webm] + <asset>-poster.png
  // clip: intrinsic dims (CLS); live: the office's LAYOUT units — the vibing
  // "camera", where smaller = closer zoom. MUST match media.json's poster job
  // or the static poster and the live canvas reframe on the crossfade.
  w?: number;
  h?: number;
  variants?: ShowcaseVariant[];
  variantGroups?: VariantGroup[]; // live channel only
  timeSlider?: boolean; // live channel: exposes a time-of-day scrub control
  poster?: string; // live channel: public/demos/-relative static fallback image
  // OPTIONAL: a channel whose caption would just restate its joined feature's
  // `desc` omits it and falls back to that desc (ChannelStage's `description`).
  caption?: string;
  status: 'live' | 'soon'; // soon = dimmed placeholder monitor, no assets needed
  default?: boolean; // exactly one — the channel tuned at load
}

// The manifest's kind/status/default/asset invariants are enforced at build
// time by the showcase guard in astro.config.mjs.
export const SHOWCASE: ShowcaseChannel[] = showcaseData as unknown as ShowcaseChannel[];

// features.json is the TOTAL set (also the README Features table's source, via
// gen-readme.mjs), partitioned by has-demo: rows carrying `channel` are exactly
// the Studio Wall dial, the `!channel` half is Showcase.astro's roster. The
// channel↔feature bijection is enforced by astro.config.mjs's build-time guard.
export interface Feature {
  icon: string;
  name: string;
  desc: string;
  featured?: boolean;
  pix?: string;
  channel?: string; // joins to a SHOWCASE row's `id`
  card?: { blurb: string };
  protocols?: string[];
}
export const FEATURES: Feature[] = (featuresData as Feature[]).map((f) => ({
  ...f,
  desc: featureText(f.desc, f),
  card: f.card && { blurb: featureText(f.card.blurb, f) },
}));

const featureByChannelId = new Map(FEATURES.filter((f) => f.channel).map((f) => [f.channel!, f]));

// The `!` is safe: the astro.config.mjs bijection guard fails the build before
// a channel without a joined feature can reach here.
export function featureForChannel(id: string): Feature {
  return featureByChannelId.get(id)!;
}

export interface EnrichedVariantGroup {
  key: string;
  label: string;
  retint: boolean;
  variants: ShowcaseVariant[];
}

export interface EnrichedShowcaseChannel extends ShowcaseChannel {
  ch: string; // zero-padded channel number, from manifest order
  variants: ShowcaseVariant[];
  groups: EnrichedVariantGroup[];
  featureDesc: string; // joined features.json row's desc — dial accordion body + caption fallback
}

// `src` is dead weight: ChannelStage's live-chip branch renders only
// id/name/accent and never reads it — set only so ShowcaseVariant stays uniform.
function variantsForRef(ref: 'themes' | 'weather'): ShowcaseVariant[] {
  if (ref === 'themes')
    return THEMES.map((t) => ({
      id: t.id,
      name: t.name,
      src: `theme_${t.id}.png`,
      accent: t.accent,
      accent2: t.accent2,
      featured: t.featured,
    }));
  return WEATHERS.map((w) => ({
    id: w.id,
    name: w.name,
    src: `weather_${w.id}.png`,
    // storm is the most striking opener for the weather channel
    featured: w.id === 'storm',
  }));
}

export function showcaseVariants(c: ShowcaseChannel): ShowcaseVariant[] {
  return c.variants ?? [];
}

export function showcaseGroups(c: ShowcaseChannel): EnrichedVariantGroup[] {
  return (c.variantGroups ?? []).map((g: VariantGroup) => ({
    key: g.key,
    label: g.label,
    retint: g.retint ?? false,
    variants: variantsForRef(g.variantsRef),
  }));
}

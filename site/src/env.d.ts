// Injected at build time: the latest release TAG, or the workspace Cargo.toml
// version as the tag-less fallback (config/released-version.mjs via astro.config.mjs).
declare const __PIXTUOID_VERSION__: string;
// Build-time GitHub star count, or null when the API was unreachable at build
// (offline builds must not fail) — consumers omit the count then.
declare const __GH_STARS__: string | null;

// The page's cross-component runtime contracts (README.md "Cross-component
// seams"). All optional: each consumer guards, and reduced-motion / pre-boot
// states leave some unset.
interface Window {
  /** THE site clock boundary (7/19) — defined in Base.astro's head boot. */
  __pixNight?: () => boolean;
  /** Per-frame dimmer opacity — written by OfficeBackdrop's controller. */
  __pixLights?: number;
  /** Hire a coworker into the live office — set once the wasm office boots.
   * Resolves to whether the engine admitted the hire (`Office::hire`'s contract). */
  __pixHire?: () => Promise<boolean>;
  /** Boot splash lifted (mirrors the one-shot pix:revealed for a late listener);
   * set by Base.astro. Gates OfficeBackdrop's office-reveal roll. */
  __pixRevealed?: boolean;
  /** Office boot RESOLVED (live / failed / unsupported) — set by OfficeBackdrop. */
  __pixEngineReady?: boolean;
  /** office-driver.js's officeHost memo. */
  __pixOfficeHost?: Promise<unknown> | null;
  /** THE theme registry + fallback, seeded parse-first in Base.astro's head. */
  __pixTheme?: {
    KEY: string;
    VALID: readonly string[];
    BG: Record<string, string>;
    ok: (_v: string) => boolean;
    fallback: () => string;
  };
  /** Key-shortcut guards (Base.astro): the typing-surface check shared by every
   * single-char shortcut, and the WCAG 2.1.4 focus gate that only `t` rides —
   * the digit shortcuts are document-global, gated by enabled()/typing(). */
  __pixKeys?: {
    typing: (_e: Event) => boolean;
    shortcutContext: () => boolean;
    /** WCAG 2.1.4 off-switch for the bare digit shortcuts, persisted in
     * localStorage('pix-keys'). */
    enabled: () => boolean;
    setEnabled: (_on: boolean) => void;
  };
}

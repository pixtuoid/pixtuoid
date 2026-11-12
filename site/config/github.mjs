// The repo's GitHub URLs and build-time star count (`__GH_STARS__`). The hash-CSP
// forbids runtime fetches, so the star fetch runs ONCE at build (astro.config.mjs),
// and an offline/local build must never fail on it: every error path yields null.
import process from 'node:process';

// The repository's slug: the site's GitHub links and calls derive from it, and
// github.test.mjs pins the copies no import reaches, in install.json and Cargo.toml.
export const REPO_SLUG = 'pixtuoid/pixtuoid';
export const REPO = `https://github.com/${REPO_SLUG}`;
export const REPO_BLOB = `${REPO}/blob/main`;
export const GH_API_REPO = `https://api.github.com/repos/${REPO_SLUG}`;
// Shared with Statusline.astro's PR-feed fetch, so an offline/slow CI runner
// fails every build-time GitHub call the same way.
export const GH_FETCH_TIMEOUT_MS = 5000;

// An unauthenticated runner's API call rate-limits to null, which hides the chip
// the e2e suite asserts.
export const E2E_STAR_COUNT = '842';

/**
 * @param {typeof fetch} [fetchImpl]
 * @param {string | undefined} [token]
 * @returns {Promise<string | null>} the count as a string ("342"), or null
 */
export async function fetchStarCount(fetchImpl = fetch, token = process.env.GITHUB_TOKEN) {
  // astro.config.mjs calls this with no seam to inject a fetchImpl stub, so an
  // e2e build substitutes the count here instead, no network. Set-but-empty
  // behaves as unset (repo convention, e.g. RUST_LOG=).
  if (process.env.GH_STARS_E2E) return E2E_STAR_COUNT;
  try {
    /** @type {Record<string, string>} */
    const headers = { accept: 'application/vnd.github+json' };
    if (token) headers.authorization = `Bearer ${token}`;
    const res = await fetchImpl(GH_API_REPO, {
      headers,
      signal: AbortSignal.timeout(GH_FETCH_TIMEOUT_MS),
    });
    if (!res.ok) return null;
    const repo = await res.json();
    const n = repo?.stargazers_count;
    return Number.isFinite(n) ? String(n) : null;
  } catch {
    return null;
  }
}

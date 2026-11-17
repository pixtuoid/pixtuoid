// A rendered Markdown file's repo-relative links: one to a file the site renders
// becomes that page's route, so a reader stays on the site; any other resolves to
// the file on GitHub.
import { posix, relative, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { visit } from 'unist-util-visit';
import { withBase } from './base-path.mjs';
import { REPO_BLOB } from './github.mjs';

const REPO_ROOT = fileURLToPath(new URL('../../', import.meta.url));

// An absolute URL: one with a scheme, or protocol-relative.
const ABSOLUTE = /^(?:[a-z][a-z0-9+.-]*:|\/\/)/i;

/**
 * @param {{ pages: readonly { path: string, route: string }[], base: string }} options
 *   every rendered Markdown file (blog.mjs's `markdownPages`) and the site's base path
 */
export default function rehypeRepoLinks({ pages, base }) {
  const routes = new Map(pages.map((p) => [p.path, withBase(base, p.route)]));
  /** @param {string} dir the linking file's directory, from the repo root */
  const rewrite = (dir) => (/** @type {any} */ node) => {
    if (node.tagName !== 'a' || typeof node.properties?.href !== 'string') return;
    const href = node.properties.href;
    if (href.startsWith('#') || ABSOLUTE.test(href)) return;
    const at = href.indexOf('#');
    const [path, hash] = at < 0 ? [href, ''] : [href.slice(0, at), href.slice(at)];
    // GitHub resolves a Markdown link "relative to the current file", and one that starts with
    // a `/` from the repository root ("Relative links and image paths in markdown files",
    // docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/customizing-your-repository/about-readmes);
    // a climb above the root is clamped.
    const joined = path.startsWith('/') ? path : posix.join(dir, path);
    const rel = posix
      .normalize(joined)
      .replace(/^(?:\.\.(?:\/|$))+/, '')
      .replace(/^\/+/, '');
    node.properties.href = `${routes.get(rel) ?? `${REPO_BLOB}/${rel}`}${hash}`;
  };
  /** @param {any} tree @param {{ path: string }} file */
  return (tree, file) => {
    const dir = posix.dirname(relative(REPO_ROOT, file.path).split(sep).join(posix.sep));
    visit(tree, 'element', rewrite(dir));
  };
}

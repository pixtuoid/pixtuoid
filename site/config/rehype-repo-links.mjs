// A rendered Markdown file's repo-relative links: one to a rendered doc becomes
// that doc's route, so a reader stays on the site; any other resolves to the
// file on GitHub, since the site serves none of the repository.
import { posix, relative, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { visit } from 'unist-util-visit';
import { withBase } from './base-path.mjs';
import { DOC_DIR } from './docs.mjs';
import { REPO_BLOB } from './github.mjs';

const REPO_ROOT = fileURLToPath(new URL('../../', import.meta.url));

// An absolute URL: one with a scheme, or protocol-relative.
const ABSOLUTE = /^(?:[a-z][a-z0-9+.-]*:|\/\/)/i;

/**
 * @param {{ docs: readonly { file: string, route: string }[], base: string }} options
 *   the rendered-doc roster and the site's base path
 */
export default function rehypeRepoLinks({ docs, base }) {
  const routes = new Map(docs.map((d) => [posix.join(DOC_DIR, d.file), withBase(base, d.route)]));
  /** @param {string} dir the linking file's directory, from the repo root */
  const rewrite = (dir) => (/** @type {any} */ node) => {
    if (node.tagName !== 'a' || typeof node.properties?.href !== 'string') return;
    const href = node.properties.href;
    if (href.startsWith('#') || ABSOLUTE.test(href)) return;
    const [path, hash = ''] = href.split(/(?=#)/);
    // A file's links resolve from its own directory, as GitHub resolves them; a climb above the
    // repo root is clamped.
    const joined = path.startsWith('/') ? path : posix.join(dir, path);
    const rel = posix
      .normalize(joined)
      .replace(/^(?:\.\.\/)+/, '')
      .replace(/^\/+/, '');
    node.properties.href = `${routes.get(rel) ?? `${REPO_BLOB}/${rel}`}${hash}`;
  };
  /** @param {any} tree @param {{ path: string }} file */
  return (tree, file) => {
    const dir = posix.dirname(relative(REPO_ROOT, file.path).split(sep).join(posix.sep));
    visit(tree, 'element', rewrite(dir));
  };
}

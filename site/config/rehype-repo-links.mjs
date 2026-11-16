// A rendered doc's repo-relative links: one to another rendered doc becomes
// that doc's route, so a reader stays on the site; any other resolves to the
// file on GitHub, since the site serves none of the repository.
import { posix } from 'node:path';
import { visit } from 'unist-util-visit';
import { withBase } from './base-path.mjs';
import { DOC_DIR } from './docs.mjs';
import { REPO_BLOB } from './github.mjs';

// An absolute URL: one with a scheme, or protocol-relative.
const ABSOLUTE = /^(?:[a-z][a-z0-9+.-]*:|\/\/)/i;

/**
 * @param {{ docs: readonly { file: string, route: string }[], base: string }} options
 *   the rendered-doc roster and the site's base path
 */
export default function rehypeRepoLinks({ docs, base }) {
  const routes = new Map(docs.map((d) => [posix.join(DOC_DIR, d.file), withBase(base, d.route)]));
  /** @param {any} node */
  const rewrite = (node) => {
    if (node.tagName !== 'a' || typeof node.properties?.href !== 'string') return;
    const href = node.properties.href;
    if (href.startsWith('#') || ABSOLUTE.test(href)) return;
    const [path, hash = ''] = href.split(/(?=#)/);
    // A doc's links resolve from its own directory; a climb above the repo root is clamped.
    const joined = path.startsWith('/') ? path : posix.join(DOC_DIR, path);
    const rel = posix
      .normalize(joined)
      .replace(/^(?:\.\.\/)+/, '')
      .replace(/^\/+/, '');
    node.properties.href = `${routes.get(rel) ?? `${REPO_BLOB}/${rel}`}${hash}`;
  };
  /** @param {any} tree */
  return (tree) => visit(tree, 'element', rewrite);
}

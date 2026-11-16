// A rendered doc's repo-relative links: one to another rendered doc becomes
// that doc's route, so a reader stays on the site; any other resolves to the
// file on GitHub, since the site serves none of the repository.
import { posix } from 'node:path';
import { visit } from 'unist-util-visit';
import { REPO_BLOB } from './github.mjs';

const DOC_DIR = 'docs'; // the rendered docs live in docs/, so links resolve from there
const SCHEME = /^[a-z][a-z0-9+.-]*:/i;

/**
 * @param {{ docs: readonly { file: string, route: string }[], base: string }} options
 *   the rendered-doc roster and the site's base path
 */
export default function rehypeRepoLinks({ docs, base }) {
  const routes = new Map(docs.map((d) => [posix.join(DOC_DIR, d.file), `${base}${d.route}`]));
  /** @param {any} node */
  const rewrite = (node) => {
    if (node.tagName !== 'a' || typeof node.properties?.href !== 'string') return;
    const href = node.properties.href;
    if (href.startsWith('#') || SCHEME.test(href)) return;
    const [path, hash = ''] = href.split(/(?=#)/);
    // resolve from docs/, clamp any climb above the repo root
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

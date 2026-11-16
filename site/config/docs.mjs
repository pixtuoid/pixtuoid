// The rendered docs' two shared facts, in a module the build, the unit tests
// and the Playwright specs can all import (src/consts.ts reads `import.meta.env`).

// The repository directory that holds the rendered docs.
export const DOC_DIR = 'docs';

/**
 * A doc page's name: its `[doc]` route param, and so its directory in dist/.
 * @param {{ route: string }} doc
 */
export const docSlug = (doc) => doc.route.replace(/\/$/, '');

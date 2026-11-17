// Two facts the rendered pages share, the docs' directory and a page's name in dist/, in a module the build, the unit tests
// and the Playwright specs can all import (src/consts.ts reads `import.meta.env`).

// The repository directory that holds the rendered docs.
export const DOC_DIR = 'docs';

/**
 * A rendered page's name, its directory in dist/: a doc's is its `[doc]` route param.
 * @param {{ route: string }} page
 */
export const pageName = (page) => page.route.replace(/\/$/, '');

// The blog's facts and the site's rosters of rendered pages (`markdownPages`, `readingRoutes`),
// in a module the build, the feed, the unit tests and
// the Playwright specs can all import (src/consts.ts reads `import.meta.env`).
import { readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { DOC_DIR } from './docs.mjs';
import docs from '../src/docs.json' with { type: 'json' };

// The site's directory in the repository, and the directory under it that holds one Markdown
// file per post.
export const SITE_DIR = 'site';
export const BLOG_DIR = 'src/content/blog';

// The blog's lobby, base-path-relative and slashed as its canonical URL.
export const BLOG_ROUTE = 'blog/';

// The feed's file, base-path-relative, and the title the feed and each page's link to it carry.
export const FEED_ROUTE = 'rss.xml';
export const BLOG_TITLE = 'pixtuoid blog';
export const BLOG_DESCRIPTION =
  'Notes from building pixtuoid: what we measured, what we changed and what did not work.';

// A post's file extension: its id is the file's name without it.
const POST_EXT = '.md';
export const POST_PATTERN = `*${POST_EXT}`;

/** @param {string} file */
export const postId = (file) => file.slice(0, -POST_EXT.length);

/** Under site/. @param {string} id */
export const postFile = (id) => `${BLOG_DIR}/${id}${POST_EXT}`;

/** @param {Date} a @param {Date} b */
export const newestFirst = (a, b) => b.valueOf() - a.valueOf();

/** @param {string} id */
export const postRoute = (id) => `${BLOG_ROUTE}${id}/`;

/** Every post's id, read from disk for what cannot ask the content collection. */
export const postIds = () =>
  readdirSync(fileURLToPath(new URL(`../${BLOG_DIR}/`, import.meta.url)))
    .filter((name) => name.endsWith(POST_EXT))
    .map(postId);

/** Every Markdown file the site renders as a page, the docs then the posts: its path from the repo root and its route. */
export const markdownPages = () => [
  ...docs.map((d) => ({ path: `${DOC_DIR}/${d.file}`, route: d.route })),
  ...postIds().map((id) => ({ path: `${SITE_DIR}/${postFile(id)}`, route: postRoute(id) })),
];

/** Every reading page's route: the blog's lobby and each rendered Markdown page. */
export const readingRoutes = () => [BLOG_ROUTE, ...markdownPages().map(({ route }) => route)];

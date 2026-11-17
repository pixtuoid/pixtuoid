// The blog's shared facts, in a module the build, the feed, the unit tests and
// the Playwright specs can all import (src/consts.ts reads `import.meta.env`).
import { readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

// The directory, under site/, that holds one Markdown file per post.
export const BLOG_DIR = 'src/content/blog';

// The blog's lobby, base-path-relative and slashed as its canonical URL.
export const BLOG_ROUTE = 'blog/';

// The feed's file, base-path-relative, and what the feed and the lobby call the blog.
export const FEED_ROUTE = 'rss.xml';
export const BLOG_TITLE = 'pixtuoid blog';
export const BLOG_DESCRIPTION =
  'Notes from building pixtuoid: what we measured, what we changed and what did not work.';

// A post's file extension: its id is the file's name without it.
const POST_EXT = '.md';
export const POST_PATTERN = `*${POST_EXT}`;

/** A post's id, from its file's name. @param {string} file */
export const postId = (file) => file.slice(0, -POST_EXT.length);

/** The order the blog lists its posts in. @param {Date} a @param {Date} b */
export const newestFirst = (a, b) => b.valueOf() - a.valueOf();

/** A post's route, from its id. @param {string} id */
export const postRoute = (id) => `${BLOG_ROUTE}${id}/`;

/** Every post's id, read from disk for what cannot ask the content collection. */
export const postIds = () =>
  readdirSync(fileURLToPath(new URL(`../${BLOG_DIR}/`, import.meta.url)))
    .filter((name) => name.endsWith(POST_EXT))
    .map(postId);

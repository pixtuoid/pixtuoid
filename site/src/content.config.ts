import { defineCollection } from 'astro:content';
import { glob } from 'astro/loaders';
import { z } from 'astro/zod';
import { DOCS } from './consts';
import { BLOG_DIR, POST_PATTERN, postId } from '../config/blog.mjs';
import { DOC_DIR } from '../config/docs.mjs';

export const collections = {
  docs: defineCollection({
    loader: glob({
      pattern: DOCS.map((d) => d.file),
      base: `../${DOC_DIR}`,
      // An entry's id is its roster `file`, so a page looks its entry up by its `file`.
      generateId: ({ entry }) => entry,
    }),
  }),
  blog: defineCollection({
    loader: glob({
      pattern: POST_PATTERN,
      base: `./${BLOG_DIR}`,
      // Astro's default is the frontmatter `slug`, else the name slugged (`generateIdDefault` in
      // its content/loaders/glob.js, at package.json's version), and what reads the directory
      // (`postIds`) would then name another route.
      generateId: ({ entry }) => postId(entry),
    }),
    schema: z.object({
      title: z.string(),
      description: z.string(),
      pubDate: z.coerce.date(),
    }),
  }),
};

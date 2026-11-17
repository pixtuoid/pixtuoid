import { defineCollection } from 'astro:content';
import { glob } from 'astro/loaders';
import { z } from 'astro/zod';
import { DOCS } from './consts';
import { BLOG_DIR } from '../config/blog.mjs';
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
    loader: glob({ pattern: '*.md', base: `./${BLOG_DIR}` }),
    schema: z.object({
      title: z.string(),
      description: z.string(),
      pubDate: z.coerce.date(),
    }),
  }),
};

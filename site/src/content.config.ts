import { defineCollection } from 'astro:content';
import { glob } from 'astro/loaders';
import { DOCS } from './consts';

// `base` is relative to the site/ project root.
export const collections = {
  docs: defineCollection({
    loader: glob({ pattern: DOCS.map((d) => d.file), base: '../docs' }),
  }),
};

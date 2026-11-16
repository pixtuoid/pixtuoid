import { defineCollection } from 'astro:content';
import { glob } from 'astro/loaders';
import { DOCS } from './consts';
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
};

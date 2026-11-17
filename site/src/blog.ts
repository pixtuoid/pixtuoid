import { getCollection, type CollectionEntry } from 'astro:content';
import type { Wing } from './consts';
import { postRoute } from '../config/blog.mjs';

export type Post = CollectionEntry<'blog'>;

// Newest first: `getCollection()` promises no order
// (docs.astro.build/en/guides/content-collections/#querying-build-time-collections).
export const posts = async (): Promise<Post[]> =>
  (await getCollection('blog')).sort((a, b) => b.data.pubDate.valueOf() - a.data.pubDate.valueOf());

export const blogWing = async (): Promise<Wing> => ({
  name: 'Blog',
  bank: 'newsroom',
  path: 'blog',
  pages: (await posts()).map((post) => ({ route: postRoute(post.id), label: post.data.title })),
});

// A post's day, as its `<time datetime>` and its visible text.
export const postDay = (post: Post): string => post.data.pubDate.toISOString().slice(0, 10);

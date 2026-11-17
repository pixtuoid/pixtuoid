import { getCollection, type CollectionEntry } from 'astro:content';
import type { Wing } from './consts';
import { newestFirst, postRoute } from '../config/blog.mjs';

export type Post = CollectionEntry<'blog'>;

// Sorted: `getCollection()` promises no order
// (docs.astro.build/en/guides/content-collections/#querying-build-time-collections).
export const posts = async (): Promise<Post[]> =>
  (await getCollection('blog')).sort((a, b) => newestFirst(a.data.pubDate, b.data.pubDate));

export const blogWing = async (): Promise<Wing> => ({
  name: 'Blog',
  bank: 'newsroom',
  path: 'blog',
  turns: { back: 'newer', on: 'older' },
  pages: (await posts()).map((post) => ({ route: postRoute(post.id), label: post.data.title })),
});

// A post's day, as its `<time datetime>` and its visible text.
export const postDay = (post: Post): string => post.data.pubDate.toISOString().slice(0, 10);

import assert from 'node:assert/strict';
import { test } from 'node:test';
import { newestFirst, postId, postRoute } from './blog.mjs';

test('the blog lists its newest post first', () => {
  const days = ['2026-01-05', '2026-11-17', '2026-03-01'].map((day) => new Date(day));
  assert.deepEqual(
    days.sort(newestFirst).map((day) => day.toISOString().slice(0, 10)),
    ['2026-11-17', '2026-03-01', '2026-01-05']
  );
});

test("a post's id is its file's name, dots and capitals kept", () => {
  assert.equal(postId('Why-SIXEL-0.22.md'), 'Why-SIXEL-0.22');
  assert.equal(postRoute(postId('a-post.md')), 'blog/a-post/');
});

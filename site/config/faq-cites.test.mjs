// Each pantry FAQ answer links a heading of a repo file, and no build reads
// across that boundary, so renaming the heading or moving the file would strand
// the link silently.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import GithubSlugger from 'github-slugger';
import faq from '../src/faq.json' with { type: 'json' };

const repoFile = (rel) => fileURLToPath(new URL(`../../${rel}`, import.meta.url));

for (const { src } of faq) {
  test(`the FAQ's ${src} names a heading of a repo file`, () => {
    const [file, fragment] = src.split('#');
    assert.ok(existsSync(repoFile(file)), `${file} is not at the repo root`);
    // One slugger per file, so a repeated heading gets GitHub's numbered anchor.
    const slugger = new GithubSlugger();
    const anchors = [...readFileSync(repoFile(file), 'utf8').matchAll(/^#{1,6} (.+)$/gm)].map((m) =>
      slugger.slug(m[1])
    );
    assert.ok(anchors.includes(fragment), `#${fragment} is no heading of ${file}`);
  });
}

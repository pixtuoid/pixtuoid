import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { featureText } from './feature-text.mjs';

const features = JSON.parse(
  readFileSync(fileURLToPath(new URL('../src/features.json', import.meta.url)), 'utf8')
);

test('a row names its protocols where its text says {protocols}', () => {
  const row = { protocols: ['kitty', 'iTerm2', 'SIXEL'] };
  assert.equal(
    featureText('speaks {protocols} graphics', row),
    'speaks kitty, iTerm2, or SIXEL graphics'
  );
  assert.throws(() => featureText('speaks {protocols}', {}), /protocols/);
});

test('every features.json text renders whole', () => {
  for (const f of features) {
    for (const text of [f.desc, f.card?.blurb].filter(Boolean)) {
      assert.doesNotMatch(featureText(text, f), /\{\w+\}/, f.name);
    }
  }
});

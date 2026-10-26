import { test } from 'node:test';
import assert from 'node:assert/strict';
import { starText } from './plaque-stars.mjs';

test('a real count renders the raw untruncated number', () => {
  assert.equal(starText('842'), '★ 842');
});

test('a large count is not truncated or formatted', () => {
  assert.equal(starText('123456'), '★ 123456');
});

test('null (offline build) renders a bare star: no "null"/"undefined" leak, no trailing space', () => {
  const out = starText(null);
  assert.equal(out, '★');
});

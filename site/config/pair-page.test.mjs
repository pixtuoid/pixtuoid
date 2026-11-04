import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { rewriteCspMeta } from './csp-hashes.mjs';

// A pairing link's code rides this page's fragment, which any script on it
// could read.
test('the pair page runs no script, as the build serves it', () => {
  const source = readFileSync(new URL('../public/pair/index.html', import.meta.url), 'utf8');
  const built = rewriteCspMeta(source);
  assert.ok(built, 'a CSP meta the build rewrites');
  assert.doesNotMatch(built, /<script\b/i);
  const policy = built.match(/content-security-policy" content="([^"]*)"/)?.[1] ?? '';
  assert.match(policy, /(?:^|; )default-src 'none'(?:;|$)/);
  assert.doesNotMatch(policy, /script-src/);
});

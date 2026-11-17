import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { inlineScriptHashes, inlineStyleHashes, rewriteCspMeta } from './csp-hashes.mjs';

const sha = (s) => `'sha256-${createHash('sha256').update(s, 'utf8').digest('base64')}'`;

test('hashes inline script content verbatim', () => {
  assert.ok(inlineScriptHashes('<script>doWork()</script>').has(sha('doWork()')));
});

test('a > inside a quoted attribute value does not truncate the content', () => {
  const h = inlineScriptHashes('<script is:inline data-note="a>b">doWork()</script>');
  assert.ok(h.has(sha('doWork()')), 'must hash the real content, not b">doWork()');
  assert.ok(!h.has(sha('b">doWork()')));
});

test('parser-error end tags a browser still honors match (js/bad-tag-filter)', () => {
  // A browser ends the script at each of these; content the regex misses would
  // ship unhashed → prod-only CSP block.
  assert.ok(inlineScriptHashes('<script>doWork()</script >').has(sha('doWork()')));
  assert.ok(inlineScriptHashes('<script>doWork()</script\n>').has(sha('doWork()')));
  assert.ok(inlineScriptHashes('<script>doWork()</script foo="bar">').has(sha('doWork()')));
  assert.ok(inlineScriptHashes('<script>doWork()</script\t\n bar>').has(sha('doWork()')));
});

test('src= inside another attribute value is not treated as a real src', () => {
  const h = inlineScriptHashes('<script data-cmd="ffmpeg src=in.mp4">go()</script>');
  assert.ok(h.has(sha('go()')), 'the inline script must still be hashed');
});

test('a real src attribute skips the script (external, rides self)', () => {
  assert.equal(inlineScriptHashes('<script src="/app.js"></script>').size, 0);
});

test('data-src is not mistaken for src', () => {
  assert.ok(inlineScriptHashes('<script data-src="x">run()</script>').has(sha('run()')));
});

test("rewriteCspMeta replaces the build's hashes with the page's own", () => {
  const html =
    '<head><meta http-equiv="content-security-policy" content="script-src \'self\'; ' +
    "style-src 'self' 'sha256-OLD'\">" +
    '<script>x()</script><style>a{b:c}</style></head><body><p style="color: red">.</p></body>';
  const out = rewriteCspMeta(html);
  assert.ok(out.includes(sha('x()')), 'script-src gains the inline hash');
  assert.ok(!out.includes("'sha256-OLD'"), "the build's stale hash is dropped");
  assert.ok(
    out.includes(`style-src 'self' 'unsafe-hashes'`),
    'a style attribute needs unsafe-hashes'
  );
  assert.ok(out.includes(sha('a{b:c}')), 'the <style> element is hashed');
  assert.ok(out.includes(sha('color: red')), 'the style attribute is hashed');
});

test('a style attribute is hashed as parsed: its character references decoded', () => {
  const { hashes } = inlineStyleHashes(
    `<i style="font-family: &quot;A&amp;B&quot;; x: &#39;&#x27;"></i>`
  );
  assert.ok(hashes.has(sha(`font-family: "A&B"; x: ''`)));
});

test('style text inside a script or style body, or a data-style attribute, is not an attribute', () => {
  const { hashes, attributes } = inlineStyleHashes(
    '<script>const s = \'<b style="x">\';</script><style>q{}</style><b data-style="y">'
  );
  assert.equal(attributes, false);
  assert.deepEqual([...hashes], [sha('q{}')]);
});

test("a style= inside another attribute's quoted value is not the style attribute", () => {
  const { hashes } = inlineStyleHashes(`<a title="x style=y" style="color:red">`);
  assert.deepEqual([...hashes], [sha('color:red')]);
  const quoted = inlineStyleHashes(`<a data-x='q style="z"' href="/">`);
  assert.equal(quoted.attributes, false);
});

test('a reference the decoder cannot hash as the browser would fails the build', () => {
  assert.throws(() => inlineStyleHashes('<i style="font: a&nbsp;b">'), /&nbsp;.*ENTITIES/);
  for (const ref of ['&#x110000;', '&#0;', '&#xD800;', '&#x80;']) {
    assert.throws(
      () => inlineStyleHashes(`<i style="x: ${ref}">`),
      /HTML.s tokenizer may remap/,
      ref
    );
  }
});

test('a page with no style attribute asks no unsafe-hashes', () => {
  const html =
    '<head><meta http-equiv="content-security-policy" content="style-src \'self\'"><style>a{}</style></head>';
  assert.ok(!rewriteCspMeta(html).includes('unsafe-hashes'));
});

test('rewriteCspMeta returns null when no CSP meta is present', () => {
  assert.equal(rewriteCspMeta('<html></html>'), null);
});

test('the meta is hoisted above every script and style it governs', () => {
  const html =
    '<!DOCTYPE html><html><head><meta charset="utf-8"><style>a{}</style>' +
    '<script>early()</script>' +
    '<meta http-equiv="content-security-policy" content="script-src \'self\'">' +
    '<script>late()</script></head></html>';
  const out = rewriteCspMeta(html);
  const meta = out.search(/<meta http-equiv="content-security-policy"/i);
  for (const m of out.matchAll(/<(?:script|style)\b/gi)) {
    assert.ok(m.index > meta, `${m[0]} at ${m.index} must follow the meta at ${meta}`);
  }
  assert.ok(out.includes(sha('early()')), 'a hoisted-over script is still hashed');
});

test('the hoisted meta lands after the charset declaration, not before it', () => {
  const out = rewriteCspMeta(
    '<head><meta charset="utf-8">' +
      '<meta http-equiv="content-security-policy" content="script-src \'self\'"></head>'
  );
  assert.ok(
    out.search(/<meta charset/i) < out.search(/<meta http-equiv/i),
    'charset must stay in the first 1024 bytes the encoding sniffer reads'
  );
});

test('rewriteCspMeta throws when the meta has no head to be hoisted into', () => {
  assert.throws(
    () =>
      rewriteCspMeta('<meta http-equiv="content-security-policy" content="script-src \'self\'">'),
    /hoist/i
  );
});

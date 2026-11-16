import { test } from 'node:test';
import assert from 'node:assert/strict';
import rehypeScrollFocus from './rehype-scroll-focus.mjs';

const el = (tagName, children = [], properties = {}) => ({
  type: 'element',
  tagName,
  properties,
  children,
});
const txt = (value) => ({ type: 'text', value });
const run = (children) => {
  const tree = { type: 'root', children };
  rehypeScrollFocus()(tree);
  return tree;
};

test('a code block and a table are each a tab stop and nothing more', () => {
  const [pre, table] = run([
    el('pre', [el('code', [txt('just test')])], { className: ['language-bash'] }),
    el('table', [el('tbody')]),
  ]).children;
  for (const node of [pre, table]) {
    assert.equal(node.properties.tabIndex, 0, node.tagName);
    assert.equal(node.properties.role, undefined, `${node.tagName} keeps its own role`);
    assert.equal(node.properties.ariaLabel, undefined, node.tagName);
  }
  assert.deepEqual(pre.properties.className, ['language-bash']);
});

test('a nested table or code block is reached, and inline code is not a scroller', () => {
  const tree = run([
    el('blockquote', [el('pre', [el('code', [txt('x')])]), el('table')]),
    el('p', [el('code', [txt('inline')])]),
  ]);
  const [pre, table] = tree.children[0].children;
  assert.equal(pre.properties.tabIndex, 0);
  assert.equal(table.properties.tabIndex, 0);
  assert.deepEqual(tree.children[1].children[0].properties, {}, 'inline code stays untouched');
});

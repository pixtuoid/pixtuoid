import { test } from 'node:test';
import assert from 'node:assert/strict';
import { splitCmds } from './install-cmds.mjs';

test('a `#` line is a note, its marker stripped; the rest are commands', () => {
  assert.deepEqual(splitCmds(['# a note', 'cmd']), { notes: ['a note'], commands: ['cmd'] });
});

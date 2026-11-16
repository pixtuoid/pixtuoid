import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { DISCORD } from './discord.mjs';

const root = fileURLToPath(new URL('../../', import.meta.url));

// A source tarball has no checkout to scan.
test(
  'every tracked Discord invite is the one the footer links',
  { skip: !existsSync(`${root}.git`) },
  () => {
    // The names of the files to read: -z and --no-color print them verbatim
    // (https://git-scm.com/docs/git-grep).
    const files = execFileSync(
      'git',
      ['grep', '-lzF', '--no-color', 'https://discord.gg/', '--', ':!site/config/discord.mjs'],
      { cwd: root, encoding: 'utf8' }
    )
      .split('\0')
      .filter(Boolean);
    const invites = files.flatMap((file) =>
      [...readFileSync(`${root}${file}`, 'utf8').matchAll(/https:\/\/discord\.gg\/\w+/g)].map(
        ([invite]) => ({ file, invite })
      )
    );
    assert.ok(invites.length > 0, 'no tracked file links a Discord invite');
    for (const { file, invite } of invites) assert.equal(invite, DISCORD, file);
  }
);

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { DISCORD } from './discord.mjs';

const root = fileURLToPath(new URL('../../', import.meta.url));

// A source tarball has no checkout to scan.
test(
  'every tracked Discord invite is the one the footer links',
  { skip: !existsSync(`${root}.git`) },
  () => {
    const tracked = execFileSync(
      'git',
      ['grep', '-ohE', 'https://discord\\.gg/[[:alnum:]]+', '--', ':!site/config/discord.mjs'],
      { cwd: root, encoding: 'utf8' }
    )
      .split('\n')
      .filter(Boolean);
    assert.ok(tracked.length > 0, 'no tracked file links a Discord invite');
    for (const invite of tracked) assert.equal(invite, DISCORD);
  }
);

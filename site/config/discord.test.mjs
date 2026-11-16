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
    // File names only, NUL-separated: no personal git config changes that output.
    const files = execFileSync(
      'git',
      ['grep', '-lzF', 'https://discord.gg/', '--', ':!site/config/discord.mjs'],
      { cwd: root, encoding: 'utf8' }
    )
      .split('\0')
      .filter(Boolean);
    assert.ok(files.length > 0, 'no tracked file links a Discord invite');
    for (const file of files) {
      for (const [invite] of readFileSync(`${root}${file}`, 'utf8').matchAll(
        /https:\/\/discord\.gg\/\w+/g
      ))
        assert.equal(invite, DISCORD, file);
    }
  }
);

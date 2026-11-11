import { strict as assert } from 'node:assert';
import test from 'node:test';
import { workspacePackageField } from './workspace-package.mjs';

const TOML = `[workspace]
members = ["crates/*"]

[workspace.dependencies]
version = "0.0.0-dependency"

[workspace.package]
version      = "1.2.3"
homepage     = "https://example.dev"

[profile.release]
version = "9.9.9-profile"
`;

test("reads a key from [workspace.package], not another table's line-anchored copy", () => {
  assert.equal(workspacePackageField(TOML, 'version'), '1.2.3');
  assert.equal(workspacePackageField(TOML, 'homepage'), 'https://example.dev');
});

test('a key the table lacks fails, naming the key', () => {
  assert.throws(() => workspacePackageField(TOML, 'repository'), /repository/);
});

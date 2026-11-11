// The workspace Cargo.toml's [workspace.package] is the one source of the
// version and the site's origin (`homepage`) that the site and the README read.

/**
 * `key`'s string in `cargoToml`'s [workspace.package] table. Scoped to that
 * table so another table's line-anchored `version = "…"` can't be picked up.
 * @param {string} cargoToml
 * @param {string} key a TOML bare key
 * @returns {string}
 */
export function workspacePackageField(cargoToml, key) {
  const table = cargoToml.match(/\[workspace\.package\]([\s\S]*?)(?:\n\[|$)/)?.[1] ?? '';
  const value = table.match(new RegExp(`^${key}\\s*=\\s*"([^"]+)"`, 'm'))?.[1];
  if (!value) throw new Error(`Cargo.toml: [workspace.package] has no ${key}`);
  return value;
}

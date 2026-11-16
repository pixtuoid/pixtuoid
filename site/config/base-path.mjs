/**
 * `path` under the site's `base`, whatever slashes either carries.
 * @param {string} base
 * @param {string} path
 * @returns {string}
 */
export const withBase = (base, path) => `${base.replace(/\/$/, '')}/${path.replace(/^\//, '')}`;

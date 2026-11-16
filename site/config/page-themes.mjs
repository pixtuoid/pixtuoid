// The page's themes, in one module a Playwright spec can import: src/consts.ts
// reads `import.meta.env`, which only Vite defines.
export const VALID_THEMES = /** @type {const} */ (['day', 'night', 'dracula']);

/**
 * A hast element.
 * @param {string} tagName
 * @param {string | string[] | null} className one class, several, or none
 * @param {object[]} [children]
 * @param {Record<string, unknown>} [properties]
 */
export const el = (tagName, className, children = [], properties = {}) => ({
  type: 'element',
  tagName,
  properties: { ...(className && { className: [className].flat() }), ...properties },
  children,
});

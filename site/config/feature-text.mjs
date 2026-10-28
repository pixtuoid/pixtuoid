// A features.json row's text as the README and the site show it: one fill for
// both, so the README generator and the site can't word a row two ways.

/**
 * `text` with `{protocols}` read from `row.protocols`, which the Rust test
 * `the_site_names_every_image_protocol` holds to the code's `ImageProtocol`.
 * @param {string} text
 * @param {{ protocols?: string[] }} row
 * @returns {string}
 */
export function featureText(text, row) {
  return text.replace('{protocols}', () => {
    if (!row.protocols) throw new Error(`"${text}" names {protocols} but its row has no protocols`);
    return new Intl.ListFormat('en', { type: 'disjunction' }).format(row.protocols);
  });
}

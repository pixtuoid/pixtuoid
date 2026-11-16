// The landing page's floors, top floor down, in a module the build, the unit
// tests and the Playwright specs can import (src/consts.ts reads `import.meta.env`).
// The Statusline lift, the ElevatorShaft and every narrative section stamp their
// id / data-floor / data-floor-label / eyebrow from this ONE array, so a rename
// or an added row can't desync them.
import floors from '../src/floors.json' with { type: 'json' };

/**
 * @typedef {object} Floor
 * @property {string} id the section's id, and so its anchor
 * @property {string} label
 * @property {string} fl the data-floor stamp, lift readout and digit-key vocabulary
 */

// A floor's stamp counts up from the bottom one, 1F, so a new row renumbers the rest.
/** @type {Floor[]} */
export const FLOORS = floors.map((f, i) => ({ ...f, fl: `${floors.length - i}F` }));

/** @param {string} id */
export function floorById(id) {
  const f = FLOORS.find((x) => x.id === id);
  if (!f) throw new Error(`floors: no floor has the id "${id}"`);
  return f;
}

/** @param {Floor} f */
export const floorName = (f) => f.label.split(' — ')[0];

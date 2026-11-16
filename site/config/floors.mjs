// The landing page's floors, top floor down, in a module the build, the unit
// tests and the Playwright specs can import (src/consts.ts reads `import.meta.env`).
import floors from '../src/floors.json' with { type: 'json' };

// A floor's stamp counts up from the bottom one, 1F, so a new row renumbers the rest.
export const FLOORS = floors.map((f, i) => ({ ...f, fl: `${floors.length - i}F` }));

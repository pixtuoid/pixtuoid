// Every doc code block and table is a tab stop, so a keyboard reaches the one
// that scrolls sideways on a narrow screen in a browser that never makes a
// scroller a tab stop itself (https://bugs.webkit.org/show_bug.cgi?id=277290).
// Its own role is enough: a tab stop needs no assigned role or name
// (https://github.com/w3c/wcag/issues/3029#issuecomment-5983645242).
import { visit } from 'unist-util-visit';

const SCROLLERS = new Set(['pre', 'table']);

export default function rehypeScrollFocus() {
  return (tree) => {
    visit(tree, 'element', (node) => {
      if (SCROLLERS.has(node.tagName)) (node.properties ||= {}).tabIndex = 0;
    });
  };
}

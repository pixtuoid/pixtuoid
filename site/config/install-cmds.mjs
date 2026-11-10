// An install.json method's `cmds`, split: a `#` line is a note, which the site
// shows but never copies and the README puts in the method's label. zsh reads `#`
// as a comment in an interactive shell only under INTERACTIVE_COMMENTS, which is
// on by default in ksh and sh emulation alone
// (https://zsh.sourceforge.io/Doc/Release/Options.html#index-INTERACTIVE_005fCOMMENTS).

/**
 * @param {string[]} cmds
 * @returns {{ notes: string[], commands: string[] }}
 */
export function splitCmds(cmds) {
  return {
    notes: cmds.filter((line) => line.startsWith('#')).map((line) => line.replace(/^#\s*/, '')),
    commands: cmds.filter((line) => !line.startsWith('#')),
  };
}

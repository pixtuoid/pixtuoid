// Blocks a turn's first stop; the stop of the turn it continued goes through.
let input = "";
process.stdin.on("data", (chunk) => (input += chunk));
process.stdin.on("end", () => {
  if (JSON.parse(input).stop_hook_active) process.exit(0);
  process.stderr.write("HERMETIC-STOP: say done.");
  process.exit(2);
});

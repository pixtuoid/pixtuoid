// Blocks a child turn's first stop; the stop of the round it continued goes through.
let input = "";
process.stdin.on("data", (chunk) => (input += chunk));
process.stdin.on("end", () => {
  if (JSON.parse(input).stopHookActive) process.exit(0);
  process.stderr.write("HERMETIC-STOP: read it again.");
  process.exit(2);
});

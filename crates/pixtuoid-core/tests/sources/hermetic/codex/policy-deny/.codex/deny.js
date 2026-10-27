// Denies every approval prompt, as an operator's policy hook would.
process.stdin.resume();
process.stdin.on("end", () => {
  process.stdout.write(
    JSON.stringify({
      hookSpecificOutput: {
        hookEventName: "PermissionRequest",
        decision: { behavior: "deny", message: "HERMETIC-POLICY: denied." },
      },
    }),
  );
});

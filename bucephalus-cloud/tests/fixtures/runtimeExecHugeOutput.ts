process.stdout.write(JSON.stringify({
  status: "completed",
  exit_code: 0,
  stdout: "x".repeat(2 * 1024 * 1024),
  stderr: "",
}));

export {};

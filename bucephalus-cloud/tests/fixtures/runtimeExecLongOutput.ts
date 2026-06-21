process.stdout.write(JSON.stringify({
  status: "completed",
  exit_code: 0,
  stdout: "x".repeat(20_000),
  stderr: "warn\n",
}));

export {};

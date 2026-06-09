const input = JSON.parse(await Bun.stdin.text()) as {
  output_dir: string;
  secrets: Array<{ id: string; ref: string }>;
};

process.stderr.write(`failed for ${input.output_dir}: ${input.secrets.map((secret) => secret.ref).join(", ")}\n`);
process.exit(42);

export {};

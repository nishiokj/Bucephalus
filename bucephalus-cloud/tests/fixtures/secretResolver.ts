const input = JSON.parse(await Bun.stdin.text()) as {
  output_dir: string;
  secrets: Array<{ id: string; ref: string }>;
};

const files: Record<string, string> = {};
for (const secret of input.secrets) {
  const filename = `${secret.id}.secret`;
  await Bun.write(`${input.output_dir}/${filename}`, `resolved:${secret.ref}`);
  files[secret.id] = filename;
}

process.stdout.write(`${JSON.stringify({ files })}\n`);

export {};

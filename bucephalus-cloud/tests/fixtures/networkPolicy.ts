import { writeFile } from "node:fs/promises";
import { join } from "node:path";

const chunks: Buffer[] = [];
for await (const chunk of process.stdin) {
  chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
}

const input = JSON.parse(Buffer.concat(chunks).toString("utf8")) as { workspace_dir: string };
await writeFile(join(input.workspace_dir, "network-policy-input.json"), JSON.stringify(input, null, 2));
process.stdout.write(`${JSON.stringify({ applied: true })}\n`);

export {};

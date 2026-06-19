import { writeFile } from "node:fs/promises";
import { join } from "node:path";

const input = JSON.parse(await Bun.stdin.text()) as {
  workspace_dir: string;
  exec: {
    command: string[];
  };
};

await writeFile(join(input.workspace_dir, "exec-input.json"), JSON.stringify(input, null, 2));
process.stdout.write(`${JSON.stringify({
  status: "completed",
  exit_code: 0,
  stdout: "hello from exec\n",
  stderr: "",
})}\n`);

export {};

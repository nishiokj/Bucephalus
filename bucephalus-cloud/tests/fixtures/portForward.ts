import { writeFile } from "node:fs/promises";
import { join } from "node:path";

const input = JSON.parse(await Bun.stdin.text()) as {
  workspace_dir: string;
  port_forward: {
    protocol: string;
    target_port: number;
    local_port: number | null;
  };
};

await writeFile(join(input.workspace_dir, "port-forward-input.json"), JSON.stringify(input, null, 2));
process.stdout.write(`${JSON.stringify({
  status: "active",
  connection: {
    kind: "loopback",
    target: `${input.port_forward.protocol}:${input.port_forward.target_port}`,
    local_port: input.port_forward.local_port,
    client_reachable: Boolean(input.port_forward.local_port),
    ...(input.port_forward.local_port ? { client_endpoint: `tcp://127.0.0.1:${input.port_forward.local_port}` } : {}),
  },
})}\n`);

export {};

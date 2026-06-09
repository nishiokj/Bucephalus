#!/usr/bin/env bun

type NetworkPolicyInput = {
  attempt_id?: unknown;
  egress_hosts?: unknown;
};

class NetworkPolicyClientError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "NetworkPolicyClientError";
  }
}

export async function applyNetworkPolicy(input: NetworkPolicyInput): Promise<{
  ok: true;
  applied: boolean;
  egress_hosts?: string[];
}> {
  const attemptId = String(input.attempt_id ?? "");
  if (!/^[A-Za-z0-9_.-]+$/.test(attemptId)) {
    throw new NetworkPolicyClientError("attempt_id contains unsupported characters");
  }
  const hosts = Array.isArray(input.egress_hosts) ? input.egress_hosts : [];
  const cleaned = [...new Set(hosts.map((host) => String(host).trim().toLowerCase()).filter(Boolean))];
  if (cleaned.length === 0) {
    return { ok: true, applied: false };
  }
  for (const host of cleaned) {
    if (!/^([a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)*[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(host)
      && !/^[0-9]{1,3}(?:\.[0-9]{1,3}){3}$/.test(host)) {
      throw new NetworkPolicyClientError(`unsupported egress host '${host}'`);
    }
  }

  const root = "/var/lib/bucephalus/network-policy";
  const requestPath = `${root}/requests/${attemptId}.hosts`;
  const ackPath = `${root}/acks/${attemptId}.ack`;
  await Bun.write(requestPath, `${cleaned.join("\n")}\n`);
  const deadline = Date.now() + 45_000;
  while (Date.now() < deadline) {
    const ack = Bun.file(ackPath);
    if (await ack.exists()) {
      const text = await ack.text();
      if (text.startsWith("ok\n") || text.trim() === "ok") {
        return { ok: true, applied: true, egress_hosts: cleaned };
      }
      throw new NetworkPolicyClientError(text.trim() || "network policy enforcer failed");
    }
    await Bun.sleep(250);
  }
  throw new NetworkPolicyClientError("timed out waiting for host network policy enforcer");
}

async function readStdin(): Promise<NetworkPolicyInput> {
  return JSON.parse(await new Response(Bun.stdin.stream()).text()) as NetworkPolicyInput;
}

if (import.meta.main) {
  readStdin()
    .then((input) => applyNetworkPolicy(input))
    .then((output) => {
      process.stdout.write(`${JSON.stringify(output)}\n`);
    })
    .catch((error) => {
      process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
      process.exit(1);
    });
}

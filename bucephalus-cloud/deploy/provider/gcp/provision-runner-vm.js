#!/usr/bin/env bun
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { randomUUID } from "node:crypto";

const DEFAULT_SCOPES = [
  "https://www.googleapis.com/auth/devstorage.read_only",
  "https://www.googleapis.com/auth/logging.write",
  "https://www.googleapis.com/auth/monitoring.write",
  "https://www.googleapis.com/auth/service.management.readonly",
  "https://www.googleapis.com/auth/servicecontrol",
  "https://www.googleapis.com/auth/trace.append",
].join(",");

async function main() {
  const input = parseJson(await Bun.stdin.text(), "provision input");
  const env = process.env;
  const project = requiredEnv(env.BUCEPHALUS_GCP_PROJECT, "BUCEPHALUS_GCP_PROJECT");
  const zone = env.BUCEPHALUS_GCP_ZONE || "us-central1-a";
  const region = env.BUCEPHALUS_GCP_REGION || zone.replace(/-[a-z]$/, "");
  const machineType = chooseMachineType(input.run_requirements, env.BUCEPHALUS_GCP_MACHINE_TYPE);
  const bootDiskGb = chooseBootDiskGb(input.run_requirements, env.BUCEPHALUS_GCP_BOOT_DISK_GB);
  const image = env.BUCEPHALUS_GCP_IMAGE || "projects/debian-cloud/global/images/family/debian-12";
  const diskType = env.BUCEPHALUS_GCP_DISK_TYPE || "pd-balanced";
  const subnet = env.BUCEPHALUS_GCP_SUBNET || "default";
  const networkTier = env.BUCEPHALUS_GCP_NETWORK_TIER || "PREMIUM";
  const serviceAccount = env.BUCEPHALUS_GCP_SERVICE_ACCOUNT || "";
  const scopes = env.BUCEPHALUS_GCP_SCOPES || DEFAULT_SCOPES;
  const name = instanceName(input, env.BUCEPHALUS_GCP_INSTANCE_PREFIX || "buc-runner");

  validateMachineCapacity(machineType, input.run_requirements);

  const workerEnv = {
    ...record(input.worker_env, "worker_env"),
    DATABASE_URL: requiredEnv(env.BUCEPHALUS_WORKER_DATABASE_URL || env.DATABASE_URL, "BUCEPHALUS_WORKER_DATABASE_URL or DATABASE_URL"),
    BUCEPHALUS_CLOUD_WORKER_TOKEN: requiredEnv(env.BUCEPHALUS_CLOUD_WORKER_TOKEN, "BUCEPHALUS_CLOUD_WORKER_TOKEN"),
    BUCEPHALUS_RUNNER_PROVIDER_INSTANCE_ID: name,
    BUCEPHALUS_WORKER_ISOLATION: env.BUCEPHALUS_WORKER_ISOLATION || "reusable_vm",
  };
  copyOptionalEnv(env, workerEnv, [
    "BUCEPHALUS_WORKER_EXECUTORS",
    "BUCEPHALUS_WORKER_RESOURCES",
    "BUCEPHALUS_WORKER_MIN_FREE_BYTES",
    "BUCEPHALUS_WORKER_POLL_MS",
    "BUCEPHALUS_WORKER_LEASE_SECONDS",
    "BUCEPHALUS_WORKER_SWEEPER_MS",
    "BUCEPHALUS_CORE_RUNNER_CMD",
    "BUCEPHALUS_CLOUD_WORKER_DIR",
    "BUCEPHALUS_CLOUD_DATA_DIR",
    "BUCEPHALUS_WORKER_SECRET_DIR",
  ]);

  const tempDir = mkdtempSync(join(tmpdir(), "buc-gcp-runner-"));
  const startupScript = join(tempDir, "startup-script.sh");
  writeFileSync(startupScript, renderStartupScript(workerEnv, env, name), { mode: 0o600 });

  const labels = {
    "bucephalus-runner": "true",
    "bucephalus-pool": labelValue(input.runner_pool_id),
    "bucephalus-provision": labelValue(input.provision_request_id),
    "bucephalus-run": labelValue(input.run_id),
  };
  const metadata = {
    "enable-osconfig": "TRUE",
    "bucephalus-provision-request-id": String(input.provision_request_id || ""),
    "bucephalus-run-id": String(input.run_id || ""),
  };

  const args = [
    "compute",
    "instances",
    "create",
    name,
    `--project=${project}`,
    `--zone=${zone}`,
    `--machine-type=${machineType}`,
    `--network-interface=network-tier=${networkTier},stack-type=IPV4_ONLY,subnet=${subnet}`,
    `--metadata=${metadataFlag(metadata)}`,
    `--metadata-from-file=startup-script=${startupScript}`,
    "--maintenance-policy=MIGRATE",
    "--provisioning-model=STANDARD",
    `--scopes=${scopes}`,
    `--create-disk=auto-delete=yes,boot=yes,device-name=${name},image=${image},mode=rw,size=${bootDiskGb},type=${diskType}`,
    "--no-shielded-secure-boot",
    "--shielded-vtpm",
    "--shielded-integrity-monitoring",
    `--labels=${metadataFlag(labels)}`,
    "--reservation-affinity=any",
    "--quiet",
  ];
  if (serviceAccount) {
    args.splice(args.indexOf(`--scopes=${scopes}`), 0, `--service-account=${serviceAccount}`);
  }

  try {
    if (truthy(env.BUCEPHALUS_GCP_DRY_RUN)) {
      writeJson({
        provider_instance_id: name,
        instance_name: name,
        metadata: {
          provider: "gcp",
          dry_run: true,
          project,
          region,
          zone,
          machine_type: machineType,
          boot_disk_gb: bootDiskGb,
          image,
          tailscale_enabled: Boolean(env.BUCEPHALUS_TAILSCALE_AUTHKEY),
          command: ["gcloud", ...args],
        },
      });
      return;
    }

    const result = spawnSync("gcloud", args, { encoding: "utf8" });
    if (result.status !== 0) {
      throw new Error(`gcloud create failed: ${tail(result.stderr || result.stdout, 4000)}`);
    }
    writeJson({
      provider_instance_id: name,
      instance_name: name,
      metadata: {
        provider: "gcp",
        project,
        region,
        zone,
        machine_type: machineType,
        boot_disk_gb: bootDiskGb,
        image,
        tailscale_enabled: Boolean(env.BUCEPHALUS_TAILSCALE_AUTHKEY),
      },
    });
  } finally {
    rmSync(tempDir, { recursive: true, force: true });
  }
}

function renderStartupScript(workerEnv, env, instanceName) {
  const releaseUrl = env.BUCEPHALUS_RELEASE_URL || "";
  const releaseSha256 = env.BUCEPHALUS_RELEASE_SHA256 || "";
  const tailscaleAuthKey = env.BUCEPHALUS_TAILSCALE_AUTHKEY || "";
  const tailscaleHostname = env.BUCEPHALUS_TAILSCALE_HOSTNAME || instanceName;
  const tailscaleExtraArgs = env.BUCEPHALUS_TAILSCALE_EXTRA_ARGS || "--accept-routes";
  const tailscaleBlock = tailscaleAuthKey ? `
if ! command -v tailscale >/dev/null 2>&1; then
  curl -fsSL https://tailscale.com/install.sh | sh
fi
systemctl enable tailscaled
systemctl start tailscaled
tailscale up --auth-key=${shellQuote(tailscaleAuthKey)} --hostname=${shellQuote(tailscaleHostname)} ${tailscaleExtraArgs}
` : "";
  const releaseBlock = releaseUrl ? `
install -d -m 0755 /opt/bucephalus-release
download_bucephalus_release ${shellQuote(releaseUrl)} /tmp/bucephalus-release.tar.gz
if [[ -n ${shellQuote(releaseSha256)} ]]; then
  printf "%s  %s\\n" ${shellQuote(releaseSha256)} /tmp/bucephalus-release.tar.gz | sha256sum -c -
fi
rm -rf /opt/bucephalus-release/current
install -d -m 0755 /opt/bucephalus-release/current
tar -xzf /tmp/bucephalus-release.tar.gz -C /opt/bucephalus-release/current --strip-components=1
install -m 0755 /opt/bucephalus-release/current/bin/bucephalus /usr/local/bin/bucephalus
rm -rf /opt/bucephalus-cloud
cp -R /opt/bucephalus-release/current/bucephalus-cloud /opt/bucephalus-cloud
` : "";
  const installDocker = env.BUCEPHALUS_GCP_INSTALL_DOCKER === "false" ? "" : `
if ! command -v docker >/dev/null 2>&1; then
  apt-get install -y docker.io
fi
systemctl enable docker
systemctl start docker
`;
  const installBun = env.BUCEPHALUS_GCP_INSTALL_BUN === "false" ? "" : `
if ! command -v bun >/dev/null 2>&1; then
  curl -fsSL https://bun.sh/install | bash
  install -m 0755 /root/.bun/bin/bun /usr/local/bin/bun
fi
`;
  const lines = Object.entries(workerEnv).map(([key, value]) => `export ${key}=${shellQuote(String(value))}`);
  return `#!/usr/bin/env bash
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive

download_bucephalus_release() {
  local source_url="$1"
  local output_path="$2"
  if [[ "\${source_url}" == gs://* ]]; then
    local without_scheme bucket object encoded_object token
    without_scheme="\${source_url#gs://}"
    bucket="\${without_scheme%%/*}"
    object="\${without_scheme#*/}"
    encoded_object="$(python3 -c 'import sys, urllib.parse; print(urllib.parse.quote(sys.argv[1], safe=""))' "\${object}")"
    token="$(curl -fsSL -H 'Metadata-Flavor: Google' 'http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token' | python3 -c 'import json, sys; print(json.load(sys.stdin)["access_token"])')"
    curl -fSL -H "Authorization: Bearer \${token}" "https://storage.googleapis.com/storage/v1/b/\${bucket}/o/\${encoded_object}?alt=media" -o "\${output_path}"
  else
    curl -fsSL "\${source_url}" -o "\${output_path}"
  fi
}

apt-get update
apt-get install -y ca-certificates curl python3 tar
${tailscaleBlock}
${installDocker}
${installBun}
${releaseBlock}
if ! command -v bun >/dev/null 2>&1; then
  echo "Bun is required on the runner VM image; install Bun or use a baked image before bootstrapping." >&2
  exit 2
fi

${lines.join("\n")}

bash /opt/bucephalus-cloud/deploy/runner-vm/bootstrap-runner-vm.sh
`;
}

function chooseMachineType(requirements, configured) {
  if (configured) {
    return configured;
  }
  const req = record(requirements, "run_requirements", false);
  const cpu = positiveInt(req.cpu_count) || 2;
  const memoryMb = positiveInt(req.memory_mb) || 4096;
  if (cpu <= 2 && memoryMb <= 4096) {
    return "e2-medium";
  }
  const needed = Math.max(cpu, Math.ceil(memoryMb / 4096), 2);
  const shape = [2, 4, 8, 16, 32].find((candidate) => candidate >= needed) || 32;
  return `e2-standard-${shape}`;
}

function chooseBootDiskGb(requirements, configured) {
  const explicit = positiveInt(configured);
  if (explicit) {
    return explicit;
  }
  const req = record(requirements, "run_requirements", false);
  const diskMb = positiveInt(req.disk_mb) || 20480;
  return Math.max(64, Math.ceil(diskMb / 1024) + 16);
}

function validateMachineCapacity(machineType, requirements) {
  const capacity = knownMachineCapacity(machineType);
  if (!capacity) {
    console.error(`warning: unknown GCP machine type capacity for ${machineType}; provider cannot prevalidate CPU/memory`);
    return;
  }
  const req = record(requirements, "run_requirements", false);
  const cpu = positiveInt(req.cpu_count);
  const memoryMb = positiveInt(req.memory_mb);
  if (cpu && capacity.cpu_count < cpu) {
    throw new Error(`${machineType} has ${capacity.cpu_count} vCPU, but run requires ${cpu}`);
  }
  if (memoryMb && capacity.memory_mb < memoryMb) {
    throw new Error(`${machineType} has ${capacity.memory_mb} MB memory, but run requires ${memoryMb} MB`);
  }
}

function knownMachineCapacity(machineType) {
  if (machineType === "e2-medium") {
    return { cpu_count: 2, memory_mb: 4096 };
  }
  const match = /^e2-(standard|highmem|highcpu)-(\d+)$/.exec(machineType);
  if (!match) {
    return null;
  }
  const cpu = Number.parseInt(match[2], 10);
  const memoryPerCpu = match[1] === "highmem" ? 8192 : match[1] === "highcpu" ? 1024 : 4096;
  return { cpu_count: cpu, memory_mb: cpu * memoryPerCpu };
}

function instanceName(input, prefix) {
  const id = String(input.provision_request_id || randomUUID()).toLowerCase().replace(/[^a-z0-9-]/g, "-");
  const base = `${prefix}-${id}`.toLowerCase().replace(/[^a-z0-9-]/g, "-").replace(/^-+|-+$/g, "");
  return base.slice(0, 63).replace(/-+$/g, "") || `buc-runner-${Date.now()}`;
}

function copyOptionalEnv(source, target, names) {
  for (const name of names) {
    if (source[name]) {
      target[name] = source[name];
    }
  }
}

function record(value, name, required = true) {
  if (value && typeof value === "object" && !Array.isArray(value)) {
    return value;
  }
  if (required) {
    throw new Error(`${name} must be a JSON object`);
  }
  return {};
}

function parseJson(raw, name) {
  try {
    return record(JSON.parse(raw), name);
  } catch (error) {
    throw new Error(`invalid ${name}: ${error instanceof Error ? error.message : String(error)}`);
  }
}

function requiredEnv(value, name) {
  if (!value || value.trim().length === 0) {
    throw new Error(`${name} is required`);
  }
  return value.trim();
}

function positiveInt(value) {
  const parsed = Number.parseInt(String(value || ""), 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : null;
}

function labelValue(value) {
  const cleaned = String(value || "none").toLowerCase().replace(/[^a-z0-9_-]/g, "-").slice(0, 63);
  return cleaned.replace(/^[^a-z0-9]+|[^a-z0-9]+$/g, "") || "none";
}

function metadataFlag(values) {
  return Object.entries(values).map(([key, value]) => `${key}=${value}`).join(",");
}

function shellQuote(value) {
  return `'${String(value).replace(/'/g, "'\"'\"'")}'`;
}

function truthy(value) {
  return ["1", "true", "yes", "on"].includes(String(value || "").toLowerCase());
}

function writeJson(value) {
  process.stdout.write(`${JSON.stringify(value)}\n`);
}

function tail(value, maxBytes) {
  const buffer = Buffer.from(String(value || ""), "utf8");
  if (buffer.byteLength <= maxBytes) {
    return buffer.toString("utf8");
  }
  return buffer.subarray(buffer.byteLength - maxBytes).toString("utf8");
}

main().catch((error) => {
  console.error(error instanceof Error ? error.message : String(error));
  process.exit(1);
});

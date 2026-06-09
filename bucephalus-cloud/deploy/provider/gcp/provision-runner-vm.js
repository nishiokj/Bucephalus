#!/usr/bin/env bun
import {
  ProviderError,
  assertDigestRef,
  assertGcpName,
  assertSimpleToken,
  googleJson,
  integerEnv,
  isRecord,
  labelValue,
  optionalEnv,
  providerInstanceId,
  readJsonStdin,
  registryHost,
  requiredEnv,
  shellQuote,
  shortId,
  waitForZoneOperation,
} from "./gce-provider-common.js";

async function main() {
  const input = await readJsonStdin();
  const config = loadConfig();
  validateRequest(input);

  const provisionRequestId = requiredString(input.provision_request_id, "/provision_request_id");
  const runnerPoolId = requiredString(input.runner_pool_id, "/runner_pool_id");
  const runId = requiredString(input.run_id, "/run_id");
  const instanceName = `${config.namePrefix}-${shortId(provisionRequestId)}`;
  assertGcpName(instanceName, "generated instance name");

  const providerId = providerInstanceId(config.projectId, config.zone, instanceName);
  const startupScript = renderStartupScript({
    apiUrl: requiredString(input.api_url, "/api_url"),
    provisionRequestId,
    runnerPoolId,
    providerId,
    instanceName,
    runnerIsolation: runnerIsolation(input),
    networkPolicyEnabled: runRequiresNetworkPolicy(input) ? "true" : "false",
    workerImage: config.workerImage,
    workerTokenSecret: config.workerTokenSecret,
    workerTokenSecretVersion: config.workerTokenSecretVersion,
    projectId: config.projectId,
    registryHost: registryHost(config.workerImage),
  });

  const body = {
    name: instanceName,
    labels: {
      app: "bucephalus-cloud",
      environment: labelValue(config.environment),
      runner_pool: labelValue(runnerPoolId),
      provision: labelValue(provisionRequestId),
    },
    tags: {
      items: config.networkTags,
    },
    machineType: `zones/${config.zone}/machineTypes/${config.machineType}`,
    disks: [{
      boot: true,
      autoDelete: true,
      initializeParams: {
        sourceImage: config.bootImage,
        diskSizeGb: String(config.bootDiskSizeGb),
        diskType: `zones/${config.zone}/diskTypes/pd-balanced`,
      },
    }],
    networkInterfaces: [{
      subnetwork: `projects/${config.projectId}/regions/${config.region}/subnetworks/${config.subnet}`,
    }],
    serviceAccounts: [{
      email: config.runnerServiceAccountEmail,
      scopes: ["https://www.googleapis.com/auth/cloud-platform"],
    }],
    metadata: {
      items: [
        { key: "startup-script", value: startupScript },
        { key: "bucephalus-runner-pool-id", value: runnerPoolId },
        { key: "bucephalus-provision-request-id", value: provisionRequestId },
        { key: "bucephalus-run-id", value: runId },
        { key: "bucephalus-worker-image", value: config.workerImage },
      ],
    },
    scheduling: {
      automaticRestart: false,
      onHostMaintenance: "MIGRATE",
      provisioningModel: "STANDARD",
    },
    shieldedInstanceConfig: {
      enableSecureBoot: true,
      enableVtpm: true,
      enableIntegrityMonitoring: true,
    },
  };

  const operation = await googleJson(
    `https://compute.googleapis.com/compute/v1/projects/${encodeURIComponent(config.projectId)}/zones/${encodeURIComponent(config.zone)}/instances`,
    {
      method: "POST",
      body: JSON.stringify(body),
    },
  );
  await waitForZoneOperation(config.projectId, config.zone, operation.name, config.operationTimeoutMs);

  console.log(JSON.stringify({
    provider_instance_id: providerId,
    instance_name: instanceName,
    metadata: {
      provider: "gcp-gce-per-run-v1",
      project_id: config.projectId,
      zone: config.zone,
      machine_type: config.machineType,
      runner_image: config.workerImage,
      no_external_ip: true,
    },
  }));
}

function loadConfig() {
  const projectId = requiredEnv("BUCEPHALUS_GCP_PROJECT_ID");
  const region = requiredEnv("BUCEPHALUS_GCP_REGION");
  const zone = requiredEnv("BUCEPHALUS_GCP_ZONE");
  const environment = optionalEnv("BUCEPHALUS_GCP_ENVIRONMENT", optionalEnv("BUCEPHALUS_ENVIRONMENT", "bucephalus"));
  const resourcePrefix = optionalEnv("BUCEPHALUS_GCP_RESOURCE_PREFIX", "buc");
  const workerImage = requiredEnv("BUCEPHALUS_GCP_RUNNER_IMAGE");
  assertDigestRef(workerImage, "BUCEPHALUS_GCP_RUNNER_IMAGE");

  return {
    projectId,
    region,
    zone,
    environment,
    namePrefix: `${resourcePrefix}-${environment}-runner`,
    workerImage,
    subnet: requiredEnv("BUCEPHALUS_GCP_SUBNET"),
    machineType: optionalEnv("BUCEPHALUS_GCP_RUNNER_MACHINE_TYPE", "e2-standard-2"),
    bootDiskSizeGb: integerEnv("BUCEPHALUS_GCP_RUNNER_BOOT_DISK_SIZE_GB", 100),
    bootImage: optionalEnv("BUCEPHALUS_GCP_RUNNER_BOOT_IMAGE", "projects/cos-cloud/global/images/family/cos-stable"),
    runnerServiceAccountEmail: requiredEnv("BUCEPHALUS_GCP_RUNNER_SERVICE_ACCOUNT_EMAIL"),
    workerTokenSecret: requiredEnv("BUCEPHALUS_GCP_WORKER_TOKEN_SECRET"),
    workerTokenSecretVersion: requiredEnv("BUCEPHALUS_GCP_WORKER_TOKEN_SECRET_VERSION"),
    networkTags: csvEnv("BUCEPHALUS_GCP_RUNNER_NETWORK_TAGS", ["bucephalus-runner"]),
    operationTimeoutMs: integerEnv("BUCEPHALUS_GCP_OPERATION_TIMEOUT_MS", 600) * 1000,
  };
}

function validateRequest(input) {
  const requirements = isRecord(input.run_requirements) ? input.run_requirements : {};
  if (requirements.executor !== undefined && requirements.executor !== "runner-docker") {
    throw new ProviderError(`GCE per-run provider only supports executor runner-docker, got ${requirements.executor}`);
  }
  if (Array.isArray(requirements.accelerators) && requirements.accelerators.length > 0) {
    throw new ProviderError("GCE per-run provider v1 does not support accelerators yet");
  }
  if (Array.isArray(requirements.sidecars) && requirements.sidecars.length > 0) {
    throw new ProviderError("GCE per-run provider v1 does not support sidecars yet");
  }
  const networkPerimeter = isRecord(requirements.network_perimeter) ? requirements.network_perimeter : {};
  if (Array.isArray(networkPerimeter.egress_hosts) && networkPerimeter.egress_hosts.length > 0) {
    if (requirements.isolation !== "single_use_vm") {
      throw new ProviderError("GCE runtime network policy enforcement requires isolation=single_use_vm");
    }
  }
}

function runRequiresNetworkPolicy(input) {
  const requirements = isRecord(input.run_requirements) ? input.run_requirements : {};
  const networkPerimeter = isRecord(requirements.network_perimeter) ? requirements.network_perimeter : {};
  return Array.isArray(networkPerimeter.egress_hosts) && networkPerimeter.egress_hosts.length > 0;
}

function runnerIsolation(input) {
  const requirements = isRecord(input.run_requirements) ? input.run_requirements : {};
  return requirements.isolation === "single_use_vm" ? "single_use_vm" : "reusable_vm";
}

function renderStartupScript(config) {
  for (const [name, value] of Object.entries(config)) {
    assertSimpleToken(String(value), name);
  }
  return `#!/usr/bin/env bash
set -euo pipefail

export DEBIAN_FRONTEND=noninteractive
PROJECT_ID=${shellQuote(config.projectId)}
API_URL=${shellQuote(config.apiUrl)}
RUNNER_POOL_ID=${shellQuote(config.runnerPoolId)}
PROVISION_REQUEST_ID=${shellQuote(config.provisionRequestId)}
PROVIDER_INSTANCE_ID=${shellQuote(config.providerId)}
INSTANCE_NAME=${shellQuote(config.instanceName)}
WORKER_IMAGE=${shellQuote(config.workerImage)}
WORKER_TOKEN_SECRET=${shellQuote(config.workerTokenSecret)}
WORKER_TOKEN_SECRET_VERSION=${shellQuote(config.workerTokenSecretVersion)}
REGISTRY_HOST=${shellQuote(config.registryHost)}
RUNNER_ISOLATION=${shellQuote(config.runnerIsolation)}
NETWORK_POLICY_ENABLED=${shellQuote(config.networkPolicyEnabled)}

metadata_token() {
  curl -fsS -H "Metadata-Flavor: Google" \\
    "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token" \\
    | sed -n 's/.*"access_token"[[:space:]]*:[[:space:]]*"\\([^"]*\\)".*/\\1/p'
}

secret_access() {
  local secret="$1"
  local version="$2"
  local token
  token="$(metadata_token)"
  curl -fsS -H "Authorization: Bearer \${token}" \\
    "https://secretmanager.googleapis.com/v1/projects/\${PROJECT_ID}/secrets/\${secret}/versions/\${version}:access" \\
    | sed -n 's/.*"data"[[:space:]]*:[[:space:]]*"\\([^"]*\\)".*/\\1/p' \\
    | base64 -d
}

ensure_host_dependencies() {
  local apt_updated="false"
  if ! command -v curl >/dev/null 2>&1; then
    if ! command -v apt-get >/dev/null 2>&1; then
      echo "curl is required on runner boot image and apt-get is unavailable" >&2
      exit 1
    fi
    apt-get update
    apt_updated="true"
    apt-get install -y --no-install-recommends ca-certificates curl
  fi
  if ! command -v docker >/dev/null 2>&1; then
    if ! command -v apt-get >/dev/null 2>&1; then
      echo "docker is required on runner boot image and apt-get is unavailable" >&2
      exit 1
    fi
    if [[ "\${apt_updated}" != "true" ]]; then
      apt-get update
    fi
    apt-get install -y --no-install-recommends docker.io
  fi
  if ! command -v iptables >/dev/null 2>&1; then
    if ! command -v apt-get >/dev/null 2>&1; then
      echo "iptables is required on runner boot image and apt-get is unavailable" >&2
      exit 1
    fi
    if [[ "\${apt_updated}" != "true" ]]; then
      apt-get update
    fi
    apt-get install -y --no-install-recommends iptables
  fi
  if command -v systemctl >/dev/null 2>&1; then
    systemctl enable --now docker || systemctl start docker
  fi
}

ensure_host_dependencies

install -d -m 0755 /var/lib/bucephalus/bin
cat >/var/lib/bucephalus/bin/gcloud <<'BUN'
#!/usr/bin/env bun
const args = process.argv.slice(2);
const version = args[3];
const secret = args[5];
const project = args[7];
if (args[0] !== "secrets" || args[1] !== "versions" || args[2] !== "access" || args[4] !== "--secret" || args[6] !== "--project") {
  console.error("unsupported gcloud subset");
  process.exit(2);
}
const tokenResponse = await fetch("http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token", {
  headers: { "Metadata-Flavor": "Google" },
});
const { access_token } = await tokenResponse.json();
const response = await fetch(\`https://secretmanager.googleapis.com/v1/projects/\${encodeURIComponent(project)}/secrets/\${encodeURIComponent(secret)}/versions/\${encodeURIComponent(version)}:access\`, {
  headers: { authorization: \`Bearer \${access_token}\` },
});
const text = await response.text();
if (!response.ok) {
  console.error(text);
  process.exit(1);
}
const payload = JSON.parse(text);
process.stdout.write(Buffer.from(payload.payload.data, "base64").toString("utf8"));
BUN
chmod 0755 /var/lib/bucephalus/bin/gcloud

cat >/var/lib/bucephalus/bin/network-policy-client <<'BUN'
#!/usr/bin/env bun
const input = JSON.parse(await new Response(Bun.stdin.stream()).text());
const attemptId = String(input.attempt_id ?? "");
if (!/^[A-Za-z0-9_.-]+$/.test(attemptId)) {
  console.error("attempt_id contains unsupported characters");
  process.exit(2);
}
const hosts = Array.isArray(input.egress_hosts) ? input.egress_hosts : [];
const cleaned = [...new Set(hosts.map((host) => String(host).trim().toLowerCase()).filter(Boolean))];
if (cleaned.length === 0) {
  process.stdout.write(JSON.stringify({ ok: true, applied: false }) + "\\n");
  process.exit(0);
}
for (const host of cleaned) {
  if (!/^([a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\\.)*[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(host) && !/^[0-9]{1,3}(?:\\.[0-9]{1,3}){3}$/.test(host)) {
    console.error(\`unsupported egress host '\${host}'\`);
    process.exit(2);
  }
}
const root = "/var/lib/bucephalus/network-policy";
const requestPath = \`\${root}/requests/\${attemptId}.hosts\`;
const ackPath = \`\${root}/acks/\${attemptId}.ack\`;
await Bun.write(requestPath, cleaned.join("\\n") + "\\n");
const deadline = Date.now() + 45_000;
while (Date.now() < deadline) {
  const ack = Bun.file(ackPath);
  if (await ack.exists()) {
    const text = await ack.text();
    if (text.startsWith("ok\\n") || text.trim() === "ok") {
      process.stdout.write(JSON.stringify({ ok: true, applied: true, egress_hosts: cleaned }) + "\\n");
      process.exit(0);
    }
    console.error(text.trim() || "network policy enforcer failed");
    process.exit(1);
  }
  await Bun.sleep(250);
}
console.error("timed out waiting for host network policy enforcer");
process.exit(1);
BUN
chmod 0755 /var/lib/bucephalus/bin/network-policy-client

cat >/var/lib/bucephalus/bin/network-policy-daemon <<'BASH'
#!/usr/bin/env bash
set -euo pipefail

ROOT=/var/lib/bucephalus/network-policy
REQ_DIR="$ROOT/requests"
ACK_DIR="$ROOT/acks"
CHAIN=BUC-EGRESS

ipt() {
  iptables -w "$@"
}

ensure_jump() {
  local iface="$1"
  ipt -C DOCKER-USER -i "$iface" -j "$CHAIN" >/dev/null 2>&1 || ipt -I DOCKER-USER 1 -i "$iface" -j "$CHAIN"
}

resolve_hosts() {
  local path="$1"
  local ips=()
  while IFS= read -r host || [[ -n "$host" ]]; do
    host="\${host%%#*}"
    host="$(printf '%s' "$host" | tr '[:upper:]' '[:lower:]' | xargs)"
    [[ -z "$host" ]] && continue
    if [[ "$host" =~ ^[0-9]{1,3}(\\.[0-9]{1,3}){3}$ ]]; then
      ips+=("$host")
      continue
    fi
    mapfile -t resolved < <(getent ahostsv4 "$host" | awk '{print $1}' | sort -u)
    if [[ "\${#resolved[@]}" -eq 0 ]]; then
      echo "could not resolve egress host: $host" >&2
      return 1
    fi
    ips+=("\${resolved[@]}")
  done <"$path"
  printf '%s\n' "\${ips[@]}" | sort -u
}

apply_policy() {
  local path="$1"
  mapfile -t ips < <(resolve_hosts "$path")
  if [[ "\${#ips[@]}" -eq 0 ]]; then
    echo "network policy request has no egress hosts" >&2
    return 1
  fi
  ipt -N "$CHAIN" >/dev/null 2>&1 || true
  ipt -F "$CHAIN"
  ipt -A "$CHAIN" -m conntrack --ctstate RELATED,ESTABLISHED -j RETURN
  ipt -A "$CHAIN" -d 172.16.0.0/12 -j RETURN
  for ip in "\${ips[@]}"; do
    ipt -A "$CHAIN" -p tcp -d "$ip" --dport 443 -j RETURN
    ipt -A "$CHAIN" -p tcp -d "$ip" --dport 80 -j RETURN
  done
  ipt -A "$CHAIN" -j REJECT
  ipt -N DOCKER-USER >/dev/null 2>&1 || true
  ensure_jump "br+"
}

mkdir -p "$REQ_DIR" "$ACK_DIR"
while true; do
  for path in "$REQ_DIR"/*.hosts; do
    [[ -e "$path" ]] || continue
    base="$(basename "$path" .hosts)"
    ack="$ACK_DIR/$base.ack"
    [[ -f "$ack" ]] && continue
    tmp="$ack.tmp"
    if apply_policy "$path" >"$tmp.out" 2>&1; then
      printf 'ok\n' >"$tmp"
    else
      { printf 'error\n'; cat "$tmp.out"; } >"$tmp"
    fi
    rm -f "$tmp.out"
    mv "$tmp" "$ack"
  done
  sleep 1
done
BASH
chmod 0755 /var/lib/bucephalus/bin/network-policy-daemon

install -d -m 0770 -o 1000 -g 1000 /var/lib/bucephalus
install -d -m 0770 -o 1000 -g 1000 /var/lib/bucephalus/network-policy
install -d -m 0770 -o 1000 -g 1000 /var/lib/bucephalus/network-policy/requests
install -d -m 0770 -o 1000 -g 1000 /var/lib/bucephalus/network-policy/acks
install -d -m 0700 /etc/bucephalus
worker_token="$(secret_access "\${WORKER_TOKEN_SECRET}" "\${WORKER_TOKEN_SECRET_VERSION}")"
worker_resources=core_runner,docker_daemon,registry_pull,secret_resolver
if [[ "\${NETWORK_POLICY_ENABLED}" == "true" ]]; then
  worker_resources="\${worker_resources},network_perimeter"
  nohup /var/lib/bucephalus/bin/network-policy-daemon >/var/log/bucephalus-network-policy.log 2>&1 &
fi
cat >/etc/bucephalus/worker.env <<EOF
BUCEPHALUS_CLOUD_API_URL=\${API_URL}
BUCEPHALUS_RUNNER_POOL_ID=\${RUNNER_POOL_ID}
BUCEPHALUS_RUNNER_PROVISION_REQUEST_ID=\${PROVISION_REQUEST_ID}
BUCEPHALUS_RUNNER_PROVIDER_INSTANCE_ID=\${PROVIDER_INSTANCE_ID}
BUCEPHALUS_WORKER_ID=\${INSTANCE_NAME}
BUCEPHALUS_CLOUD_DATA_DIR=/var/lib/bucephalus
BUCEPHALUS_CORE_RUNNER_CMD=bucephalus
BUCEPHALUS_WORKER_RESOURCES=\${worker_resources}
BUCEPHALUS_WORKER_EXECUTORS=runner-docker
BUCEPHALUS_WORKER_ISOLATION=\${RUNNER_ISOLATION}
BUCEPHALUS_WORKER_SECRET_RESOLVER_CMD_JSON=["bucephalus-cloud-secret-resolver"]
BUCEPHALUS_SECRET_RESOLVER_GCLOUD_CMD=/usr/local/bin/gcloud
EOF
if [[ "\${NETWORK_POLICY_ENABLED}" == "true" ]]; then
  printf 'BUCEPHALUS_WORKER_NETWORK_POLICY_CMD_JSON=["bucephalus-cloud-network-policy"]\\n' >>/etc/bucephalus/worker.env
fi
printf 'BUCEPHALUS_CLOUD_WORKER_TOKEN=%s\\n' "\${worker_token}" >>/etc/bucephalus/worker.env
chmod 0600 /etc/bucephalus/worker.env

metadata_token | docker login -u oauth2accesstoken --password-stdin "\${REGISTRY_HOST}"
docker pull "\${WORKER_IMAGE}"
docker rm -f bucephalus-worker >/dev/null 2>&1 || true
docker run -d \\
  --name bucephalus-worker \\
  --restart unless-stopped \\
  --env-file /etc/bucephalus/worker.env \\
  --group-add "$(stat -c '%g' /var/run/docker.sock)" \\
  -v /var/run/docker.sock:/var/run/docker.sock \\
  -v /var/lib/bucephalus:/var/lib/bucephalus \\
  -v /var/lib/bucephalus/bin/gcloud:/usr/local/bin/gcloud:ro \\
  -v /var/lib/bucephalus/bin/network-policy-client:/usr/local/bin/bucephalus-cloud-network-policy:ro \\
  "\${WORKER_IMAGE}"
`;
}

function requiredString(value, pointer) {
  if (typeof value !== "string" || value.trim().length === 0) {
    throw new ProviderError(`${pointer} is required`);
  }
  return value.trim();
}

function csvEnv(name, fallback) {
  const raw = optionalEnv(name);
  if (!raw) {
    return fallback;
  }
  return raw.split(",").map((item) => item.trim()).filter(Boolean);
}

main().catch((error) => {
  console.error(error instanceof Error ? error.message : String(error));
  process.exit(1);
});

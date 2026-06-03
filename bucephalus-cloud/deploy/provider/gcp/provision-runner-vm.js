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
      onHostMaintenance: "TERMINATE",
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
    subnet: requiredEnv("BUCEPHALUS_GCP_SUBNET"),
    machineType: optionalEnv("BUCEPHALUS_GCP_RUNNER_MACHINE_TYPE", "e2-standard-2"),
    bootDiskSizeGb: integerEnv("BUCEPHALUS_GCP_RUNNER_BOOT_DISK_SIZE_GB", 100),
    bootImage: optionalEnv("BUCEPHALUS_GCP_RUNNER_BOOT_IMAGE", "projects/ubuntu-os-cloud/global/images/family/ubuntu-2404-lts-amd64"),
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
    throw new ProviderError("GCE per-run provider v1 does not install a runtime network policy enforcer yet");
  }
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

metadata_token() {
  curl -fsS -H "Metadata-Flavor: Google" \\
    "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token" \\
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["access_token"])'
}

secret_access() {
  local secret="$1"
  local version="$2"
  local token
  token="$(metadata_token)"
  curl -fsS -H "Authorization: Bearer \${token}" \\
    "https://secretmanager.googleapis.com/v1/projects/\${PROJECT_ID}/secrets/\${secret}/versions/\${version}:access" \\
    | python3 -c 'import base64,json,sys; print(base64.b64decode(json.load(sys.stdin)["payload"]["data"]).decode(), end="")'
}

apt-get update
apt-get install -y --no-install-recommends ca-certificates curl docker.io python3
systemctl enable --now docker

install -d -m 0755 /opt/bucephalus/bin
cat >/opt/bucephalus/bin/gcloud <<'BUN'
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
chmod 0755 /opt/bucephalus/bin/gcloud

install -d -m 0770 -o 1000 -g 1000 /var/lib/bucephalus
install -d -m 0700 /etc/bucephalus
worker_token="$(secret_access "\${WORKER_TOKEN_SECRET}" "\${WORKER_TOKEN_SECRET_VERSION}")"
cat >/etc/bucephalus/worker.env <<EOF
BUCEPHALUS_CLOUD_API_URL=\${API_URL}
BUCEPHALUS_RUNNER_POOL_ID=\${RUNNER_POOL_ID}
BUCEPHALUS_RUNNER_PROVISION_REQUEST_ID=\${PROVISION_REQUEST_ID}
BUCEPHALUS_RUNNER_PROVIDER_INSTANCE_ID=\${PROVIDER_INSTANCE_ID}
BUCEPHALUS_WORKER_ID=\${INSTANCE_NAME}
BUCEPHALUS_CLOUD_DATA_DIR=/var/lib/bucephalus
BUCEPHALUS_CORE_RUNNER_CMD=bucephalus
BUCEPHALUS_WORKER_RESOURCES=core_runner,docker_daemon,registry_pull,secret_resolver
BUCEPHALUS_WORKER_EXECUTORS=runner-docker
BUCEPHALUS_WORKER_SECRET_RESOLVER_CMD_JSON=["bucephalus-cloud-secret-resolver"]
BUCEPHALUS_SECRET_RESOLVER_GCLOUD_CMD=/usr/local/bin/gcloud
EOF
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
  -v /opt/bucephalus/bin/gcloud:/usr/local/bin/gcloud:ro \\
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

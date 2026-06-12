const CONTROL_PLANE_ENV_NAMES = new Set([
  "AWS_ACCESS_KEY_ID",
  "AWS_SECRET_ACCESS_KEY",
  "AWS_SESSION_TOKEN",
  "BUCEPHALUS_CLOUD_ALLOW_CONTROL_PLANE_SECRET_REFS",
  "BUCEPHALUS_CLOUD_ALLOW_LOCAL_IMAGE_REFS",
  "BUCEPHALUS_CLOUD_API_URL",
  "BUCEPHALUS_CLOUD_WORKER_TOKEN",
  "BUCEPHALUS_POOL_CONTROLLER_PROVISION_CMD_JSON",
  "BUCEPHALUS_POOL_CONTROLLER_REAP_CMD_JSON",
  "BUCEPHALUS_RUN_STORE",
  "BUCEPHALUS_RUN_STORE_SCHEMA",
  "BUCEPHALUS_RUN_STORE_URL",
  "BUCEPHALUS_RUNNER_INSTANCE_ID",
  "BUCEPHALUS_RUNNER_POOL_ID",
  "BUCEPHALUS_RUNNER_PROVIDER_INSTANCE_ID",
  "BUCEPHALUS_RUNNER_PROVISION_REQUEST_ID",
  "BUCEPHALUS_SECRET_RESOLVER_ALLOW_CONTROL_PLANE_REFS",
  "BUCEPHALUS_SECRET_RESOLVER_ALLOW_ENV",
  "BUCEPHALUS_SECRET_RESOLVER_AWS_CMD",
  "BUCEPHALUS_SECRET_RESOLVER_GCLOUD_CMD",
  "BUCEPHALUS_SECRET_RESOLVER_GCP_AUTH",
  "BUCEPHALUS_WORKER_DATABASE_URL",
  "BUCEPHALUS_WORKER_SECRET_RESOLVER_CMD_JSON",
  "DATABASE_URL",
  "GOOGLE_APPLICATION_CREDENTIALS",
]);

const CONTROL_PLANE_SECRET_PATTERNS = [
  /(^|[-_/:])worker[-_]?token($|[-_/:])/,
  /(^|[-_/:])api[-_]?database[-_]?url($|[-_/:])/,
  /(^|[-_/:])migrator[-_]?database[-_]?url($|[-_/:])/,
  /(^|[-_/:])pool[-_](controller[-_]?)?provision[-_]?cmd[-_]?json($|[-_/:])/,
  /(^|[-_/:])pool[-_](controller[-_]?)?reap[-_]?cmd[-_]?json($|[-_/:])/,
];

export function allowsControlPlaneSecretRefs(env: NodeJS.ProcessEnv = process.env): boolean {
  return truthy(env.BUCEPHALUS_SECRET_RESOLVER_ALLOW_CONTROL_PLANE_REFS)
    || truthy(env.BUCEPHALUS_CLOUD_ALLOW_CONTROL_PLANE_SECRET_REFS);
}

export function controlPlaneSecretIdViolation(id: string): string | null {
  const normalized = id.trim().toUpperCase();
  if (CONTROL_PLANE_ENV_NAMES.has(normalized)) {
    return `Secret id '${id}' is reserved for Cloud control-plane credentials`;
  }
  return null;
}

export function controlPlaneEnvNameViolation(name: string): string | null {
  const normalized = name.trim().toUpperCase();
  if (CONTROL_PLANE_ENV_NAMES.has(normalized)) {
    return `Environment variable '${name}' is reserved for Cloud runtime/control-plane state`;
  }
  return null;
}

export function controlPlaneSecretNameViolation(name: string): string | null {
  const trimmed = name.trim();
  if (CONTROL_PLANE_ENV_NAMES.has(trimmed.toUpperCase())
    || CONTROL_PLANE_SECRET_PATTERNS.some((pattern) => pattern.test(trimmed.toLowerCase()))) {
    return `Secret name '${name}' is reserved for Cloud control-plane credentials`;
  }
  return null;
}

export function controlPlaneSecretRefViolation(ref: string): string | null {
  const envName = envRefName(ref);
  if (envName && CONTROL_PLANE_ENV_NAMES.has(envName.toUpperCase())) {
    return `Secret ref '${ref}' targets a reserved Cloud control-plane environment variable`;
  }

  const providerName = providerSecretName(ref);
  if (!providerName) {
    return null;
  }
  const normalized = providerName.trim().toLowerCase();
  if (CONTROL_PLANE_SECRET_PATTERNS.some((pattern) => pattern.test(normalized))) {
    return `Secret ref '${ref}' targets a reserved Cloud control-plane secret name`;
  }
  return null;
}

function envRefName(ref: string): string | null {
  return ref.startsWith("env:") ? ref.slice("env:".length) : null;
}

function providerSecretName(ref: string): string | null {
  const gcpPath = ref.startsWith("gcp-secret-manager://")
    ? ref.slice("gcp-secret-manager://".length)
    : ref.startsWith("gcp://")
      ? ref.slice("gcp://".length)
      : null;
  if (gcpPath !== null) {
    const match = /^projects\/[^/]+\/secrets\/([^/]+)\/versions\/[^/]+$/.exec(gcpPath);
    return match?.[1] ?? null;
  }

  const awsPrefix = "aws-secrets-manager://";
  if (ref.startsWith(awsPrefix)) {
    return ref.slice(awsPrefix.length);
  }
  return null;
}

function truthy(value: string | undefined): boolean {
  return value !== undefined && ["1", "true", "yes", "on"].includes(value.trim().toLowerCase());
}

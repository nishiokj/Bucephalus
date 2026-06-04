const CONTROL_PLANE_ENV_NAMES = new Set([
  "BUCEPHALUS_CLOUD_WORKER_TOKEN",
  "BUCEPHALUS_WORKER_DATABASE_URL",
  "DATABASE_URL",
  "BUCEPHALUS_POOL_CONTROLLER_PROVISION_CMD_JSON",
  "BUCEPHALUS_POOL_CONTROLLER_REAP_CMD_JSON",
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

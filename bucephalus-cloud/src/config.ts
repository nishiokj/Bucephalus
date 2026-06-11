export interface AppConfig {
  databaseUrl: string;
  dataDir: string;
  host: string;
  port: number;
  workerToken: string | null;
  runnerAdminToken: string | null;
  auth: AuthConfig;
  rateLimit: RateLimitConfig;
  storage: StorageConfig;
  secrets: SecretsStoreConfig;
}

export type SecretsStoreConfig =
  | {
      backend: "filesystem";
      prefix: string;
    }
  | {
      backend: "gcp";
      project: string;
      prefix: string;
    };

export interface AuthConfig {
  required: boolean;
  issuer: string | null;
  audiences: string[] | null;
  jwksUrl: string | null;
}

export interface RateLimitConfig {
  enabled: boolean;
  windowMs: number;
  ipMax: number;
  credentialMax: number;
}

export type StorageConfig =
  | {
      backend: "filesystem";
    }
  | {
      backend: "r2";
      accountId: string | null;
      endpoint: string;
      bucket: string;
      prefix: string;
      accessKeyId: string;
      secretAccessKey: string;
    }
  | {
      backend: "gcs";
      bucket: string;
      prefix: string;
    };

export function loadConfig(env: NodeJS.ProcessEnv = process.env): AppConfig {
  const issuer = env.BUCEPHALUS_CLOUD_OAUTH_ISSUER?.trim() || null;
  const audiences = parseCsv(env.BUCEPHALUS_CLOUD_OAUTH_AUDIENCE);
  const explicitJwksUrl = env.BUCEPHALUS_CLOUD_OAUTH_JWKS_URL?.trim() || null;
  const authRequired = env.BUCEPHALUS_CLOUD_AUTH_REQUIRED?.trim().toLowerCase() || null;
  if (authRequired && authRequired !== "true") {
    throw new Error(
      "BUCEPHALUS_CLOUD_AUTH_REQUIRED cannot disable user auth; configure OAuth",
    );
  }
  const required = true;
  const jwksUrl = explicitJwksUrl ?? defaultJwksUrl(issuer);

  return {
    databaseUrl:
      env.DATABASE_URL ??
      "postgres://bucephalus:bucephalus_dev@localhost:55432/bucephalus_cloud",
    dataDir: env.BUCEPHALUS_CLOUD_DATA_DIR ?? ".data",
    host: env.BUCEPHALUS_CLOUD_HOST ?? "127.0.0.1",
    port: Number.parseInt(env.PORT ?? "8080", 10),
    workerToken: env.BUCEPHALUS_CLOUD_WORKER_TOKEN?.trim() || null,
    runnerAdminToken: env.BUCEPHALUS_CLOUD_RUNNER_ADMIN_TOKEN?.trim() || null,
    auth: {
      required,
      issuer,
      audiences,
      jwksUrl,
    },
    rateLimit: loadRateLimitConfig(env),
    storage: loadStorageConfig(env),
    secrets: loadSecretsStoreConfig(env),
  };
}

function loadRateLimitConfig(env: NodeJS.ProcessEnv): RateLimitConfig {
  return {
    enabled: parseBoolean(env.BUCEPHALUS_CLOUD_RATE_LIMIT_ENABLED, true),
    windowMs: parsePositiveInteger(env.BUCEPHALUS_CLOUD_RATE_LIMIT_WINDOW_MS, 60_000),
    ipMax: parsePositiveInteger(env.BUCEPHALUS_CLOUD_RATE_LIMIT_IP_MAX, 300),
    credentialMax: parsePositiveInteger(env.BUCEPHALUS_CLOUD_RATE_LIMIT_CREDENTIAL_MAX, 120),
  };
}

function loadSecretsStoreConfig(env: NodeJS.ProcessEnv): SecretsStoreConfig {
  const backend = (env.BUCEPHALUS_CLOUD_SECRETS_BACKEND?.trim() || "filesystem").toLowerCase();
  const prefix = env.BUCEPHALUS_CLOUD_SECRETS_PREFIX?.trim() || "buc";
  if (!/^[A-Za-z0-9_-]+$/.test(prefix)) {
    throw new Error(`Invalid BUCEPHALUS_CLOUD_SECRETS_PREFIX: ${prefix}`);
  }
  if (backend === "filesystem") {
    return { backend: "filesystem", prefix };
  }
  if (backend !== "gcp") {
    throw new Error(`Unsupported BUCEPHALUS_CLOUD_SECRETS_BACKEND: ${backend}`);
  }
  const project = env.BUCEPHALUS_CLOUD_SECRETS_GCP_PROJECT?.trim() || null;
  if (!project) {
    throw new Error("BUCEPHALUS_CLOUD_SECRETS_GCP_PROJECT is required for the GCP secrets backend");
  }
  return { backend: "gcp", project, prefix };
}

function defaultJwksUrl(issuer: string | null): string | null {
  if (!issuer) {
    return null;
  }
  const normalized = issuer.replace(/\/+$/, "");
  if (normalized === "https://accounts.google.com") {
    return "https://www.googleapis.com/oauth2/v3/certs";
  }
  return `${normalized}/.well-known/jwks.json`;
}

function loadStorageConfig(env: NodeJS.ProcessEnv): StorageConfig {
  const backend = (env.BUCEPHALUS_CLOUD_STORAGE_BACKEND?.trim() || "filesystem").toLowerCase();
  if (backend === "filesystem") {
    return { backend: "filesystem" };
  }
  if (backend === "gcs") {
    const bucket = env.BUCEPHALUS_CLOUD_GCS_BUCKET?.trim() || null;
    if (!bucket) {
      throw new Error("BUCEPHALUS_CLOUD_GCS_BUCKET is required for GCS storage");
    }
    return {
      backend: "gcs",
      bucket,
      prefix: stripSlashes(env.BUCEPHALUS_CLOUD_GCS_PREFIX?.trim() ?? ""),
    };
  }
  if (backend !== "r2") {
    throw new Error(`Unsupported BUCEPHALUS_CLOUD_STORAGE_BACKEND: ${backend}`);
  }

  const accountId = env.BUCEPHALUS_CLOUD_R2_ACCOUNT_ID?.trim() || null;
  const explicitEndpoint = env.BUCEPHALUS_CLOUD_R2_ENDPOINT?.trim() || null;
  const bucket = env.BUCEPHALUS_CLOUD_R2_BUCKET?.trim() || null;
  const accessKeyId = env.BUCEPHALUS_CLOUD_R2_ACCESS_KEY_ID?.trim() || null;
  const secretAccessKey = env.BUCEPHALUS_CLOUD_R2_SECRET_ACCESS_KEY?.trim() || null;
  if (!explicitEndpoint && !accountId) {
    throw new Error("BUCEPHALUS_CLOUD_R2_ACCOUNT_ID or BUCEPHALUS_CLOUD_R2_ENDPOINT is required for R2 storage");
  }
  if (!bucket) {
    throw new Error("BUCEPHALUS_CLOUD_R2_BUCKET is required for R2 storage");
  }
  if (!accessKeyId) {
    throw new Error("BUCEPHALUS_CLOUD_R2_ACCESS_KEY_ID is required for R2 storage");
  }
  if (!secretAccessKey) {
    throw new Error("BUCEPHALUS_CLOUD_R2_SECRET_ACCESS_KEY is required for R2 storage");
  }

  return {
    backend: "r2",
    accountId,
    endpoint: stripTrailingSlashes(explicitEndpoint ?? `https://${accountId}.r2.cloudflarestorage.com`),
    bucket,
    prefix: stripSlashes(env.BUCEPHALUS_CLOUD_R2_PREFIX?.trim() ?? ""),
    accessKeyId,
    secretAccessKey,
  };
}

function stripTrailingSlashes(value: string): string {
  return value.replace(/\/+$/, "");
}

function stripSlashes(value: string): string {
  return value.replace(/^\/+|\/+$/g, "");
}

function parseCsv(value: string | undefined): string[] | null {
  const values = (value ?? "")
    .split(",")
    .map((part) => part.trim())
    .filter(Boolean);
  return values.length === 0 ? null : values;
}

function parsePositiveInteger(value: string | undefined, fallback: number): number {
  const parsed = Number.parseInt(value ?? "", 10);
  return Number.isSafeInteger(parsed) && parsed > 0 ? parsed : fallback;
}

function parseBoolean(value: string | undefined, fallback: boolean): boolean {
  if (value === undefined) {
    return fallback;
  }
  const normalized = value.trim().toLowerCase();
  if (["1", "true", "yes", "on"].includes(normalized)) {
    return true;
  }
  if (["0", "false", "no", "off"].includes(normalized)) {
    return false;
  }
  return fallback;
}

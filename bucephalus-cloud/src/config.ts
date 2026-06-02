export interface AppConfig {
  databaseUrl: string;
  dataDir: string;
  host: string;
  port: number;
  workerToken: string | null;
  auth: AuthConfig;
}

export interface AuthConfig {
  required: boolean;
  issuer: string | null;
  audience: string | null;
  jwksUrl: string | null;
  devToken: string | null;
}

export function loadConfig(env: NodeJS.ProcessEnv = process.env): AppConfig {
  const issuer = env.BUCEPHALUS_CLOUD_OAUTH_ISSUER?.trim() || null;
  const audience = env.BUCEPHALUS_CLOUD_OAUTH_AUDIENCE?.trim() || null;
  const explicitJwksUrl = env.BUCEPHALUS_CLOUD_OAUTH_JWKS_URL?.trim() || null;
  const devToken = env.BUCEPHALUS_CLOUD_OAUTH_DEV_TOKEN?.trim() || null;
  const required = env.BUCEPHALUS_CLOUD_AUTH_REQUIRED === undefined
    ? Boolean((issuer && audience) || devToken)
    : env.BUCEPHALUS_CLOUD_AUTH_REQUIRED !== "false";

  return {
    databaseUrl:
      env.DATABASE_URL ??
      "postgres://bucephalus:bucephalus_dev@localhost:55432/bucephalus_cloud",
    dataDir: env.BUCEPHALUS_CLOUD_DATA_DIR ?? ".data",
    host: env.BUCEPHALUS_CLOUD_HOST ?? "127.0.0.1",
    port: Number.parseInt(env.PORT ?? "8080", 10),
    workerToken: env.BUCEPHALUS_CLOUD_WORKER_TOKEN?.trim() || null,
    auth: {
      required,
      issuer,
      audience,
      jwksUrl: explicitJwksUrl ?? (issuer ? `${issuer.replace(/\/+$/, "")}/.well-known/jwks.json` : null),
      devToken,
    },
  };
}

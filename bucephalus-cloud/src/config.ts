export interface AppConfig {
  databaseUrl: string;
  dataDir: string;
  port: number;
}

export function loadConfig(env: NodeJS.ProcessEnv = process.env): AppConfig {
  return {
    databaseUrl:
      env.DATABASE_URL ??
      "postgres://bucephalus:bucephalus_dev@localhost:55432/bucephalus_cloud",
    dataDir: env.BUCEPHALUS_CLOUD_DATA_DIR ?? ".data",
    port: Number.parseInt(env.PORT ?? "8080", 10),
  };
}

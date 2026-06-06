import { describe, expect, test } from "bun:test";
import { loadConfig } from "../src/config";

describe("config", () => {
  test("uses Google's OAuth JWKS endpoint for Google user tokens", () => {
    const config = loadConfig({
      BUCEPHALUS_CLOUD_OAUTH_ISSUER: "https://accounts.google.com",
      BUCEPHALUS_CLOUD_OAUTH_AUDIENCE: "example-client.apps.googleusercontent.com",
    });

    expect(config.auth.jwksUrl).toBe("https://www.googleapis.com/oauth2/v3/certs");
  });

  test("preserves an explicit OAuth JWKS URL", () => {
    const config = loadConfig({
      BUCEPHALUS_CLOUD_OAUTH_ISSUER: "https://accounts.google.com",
      BUCEPHALUS_CLOUD_OAUTH_AUDIENCE: "example-client.apps.googleusercontent.com",
      BUCEPHALUS_CLOUD_OAUTH_JWKS_URL: "https://issuer.example.test/keys",
    });

    expect(config.auth.jwksUrl).toBe("https://issuer.example.test/keys");
  });

  test("loads an optional runner admin token separately from the worker token", () => {
    const config = loadConfig({
      BUCEPHALUS_CLOUD_WORKER_TOKEN: "worker-token",
      BUCEPHALUS_CLOUD_RUNNER_ADMIN_TOKEN: "runner-admin-token",
    });

    expect(config.workerToken).toBe("worker-token");
    expect(config.runnerAdminToken).toBe("runner-admin-token");
  });

  test("uses filesystem object storage by default", () => {
    expect(loadConfig({}).storage).toEqual({ backend: "filesystem" });
  });

  test("loads R2 object storage settings", () => {
    const config = loadConfig({
      BUCEPHALUS_CLOUD_STORAGE_BACKEND: "r2",
      BUCEPHALUS_CLOUD_R2_ACCOUNT_ID: "account-id",
      BUCEPHALUS_CLOUD_R2_BUCKET: "buc-artifacts",
      BUCEPHALUS_CLOUD_R2_PREFIX: "/prod/",
      BUCEPHALUS_CLOUD_R2_ACCESS_KEY_ID: "access-key",
      BUCEPHALUS_CLOUD_R2_SECRET_ACCESS_KEY: "secret-key",
    });

    expect(config.storage).toEqual({
      backend: "r2",
      accountId: "account-id",
      endpoint: "https://account-id.r2.cloudflarestorage.com",
      bucket: "buc-artifacts",
      prefix: "prod",
      accessKeyId: "access-key",
      secretAccessKey: "secret-key",
    });
  });
});

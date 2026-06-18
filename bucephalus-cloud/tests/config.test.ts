import { describe, expect, test } from "bun:test";
import { loadConfig } from "../src/config";

describe("config", () => {
  test("requires user auth by default", () => {
    expect(loadConfig({}).auth.required).toBe(true);
  });

  test("does not allow user auth to be disabled", () => {
    expect(() => loadConfig({
      BUCEPHALUS_CLOUD_AUTH_REQUIRED: "false",
    })).toThrow("BUCEPHALUS_CLOUD_AUTH_REQUIRED cannot disable user auth; configure OAuth");
  });

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

  test("loads comma-separated OAuth audiences", () => {
    const config = loadConfig({
      BUCEPHALUS_CLOUD_OAUTH_AUDIENCE: "web-client.apps.googleusercontent.com, sdk-client.apps.googleusercontent.com",
    });

    expect(config.auth.audiences).toEqual([
      "web-client.apps.googleusercontent.com",
      "sdk-client.apps.googleusercontent.com",
    ]);
  });

  test("loads separate CLI OAuth client settings", () => {
    const config = loadConfig({
      BUCEPHALUS_CLOUD_OAUTH_AUDIENCE: "web-client.apps.googleusercontent.com, sdk-client.apps.googleusercontent.com",
      BUCEPHALUS_CLOUD_OAUTH_CLI_CLIENT_ID: "sdk-client.apps.googleusercontent.com",
      BUCEPHALUS_CLOUD_OAUTH_CLI_CLIENT_SECRET: "client-secret",
      BUCEPHALUS_CLOUD_OAUTH_CLI_SCOPE: "openid email",
    });

    expect(config.auth.cliClientId).toBe("sdk-client.apps.googleusercontent.com");
    expect(config.auth.cliClientSecret).toBe("client-secret");
    expect(config.auth.cliScope).toBe("openid email");
  });

  test("loads an optional runner admin token separately from the worker token", () => {
    const config = loadConfig({
      BUCEPHALUS_CLOUD_WORKER_TOKEN: "worker-token",
      BUCEPHALUS_CLOUD_RUNNER_ADMIN_TOKEN: "runner-admin-token",
    });

    expect(config.workerToken).toBe("worker-token");
    expect(config.runnerAdminToken).toBe("runner-admin-token");
  });

  test("defaults build evidence policy to warn for local development", () => {
    expect(loadConfig({}).buildEvidencePolicy).toBe("warn");
  });

  test("loads build evidence enforcement policy", () => {
    const config = loadConfig({
      BUCEPHALUS_CLOUD_BUILD_EVIDENCE_POLICY: "enforce",
    });

    expect(config.buildEvidencePolicy).toBe("enforce");
  });

  test("rejects unknown build evidence policies", () => {
    expect(() => loadConfig({
      BUCEPHALUS_CLOUD_BUILD_EVIDENCE_POLICY: "maybe",
    })).toThrow("BUCEPHALUS_CLOUD_BUILD_EVIDENCE_POLICY must be 'warn' or 'enforce'");
  });

  test("loads rate limiting safety defaults", () => {
    expect(loadConfig({}).rateLimit).toEqual({
      enabled: true,
      windowMs: 60_000,
      ipMax: 300,
      credentialMax: 120,
    });
  });

  test("loads rate limiting overrides", () => {
    const config = loadConfig({
      BUCEPHALUS_CLOUD_RATE_LIMIT_ENABLED: "false",
      BUCEPHALUS_CLOUD_RATE_LIMIT_WINDOW_MS: "5000",
      BUCEPHALUS_CLOUD_RATE_LIMIT_IP_MAX: "25",
      BUCEPHALUS_CLOUD_RATE_LIMIT_CREDENTIAL_MAX: "10",
    });

    expect(config.rateLimit).toEqual({
      enabled: false,
      windowMs: 5000,
      ipMax: 25,
      credentialMax: 10,
    });
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

  test("loads GCS object storage settings", () => {
    const config = loadConfig({
      BUCEPHALUS_CLOUD_STORAGE_BACKEND: "gcs",
      BUCEPHALUS_CLOUD_GCS_BUCKET: "buc-artifacts",
      BUCEPHALUS_CLOUD_GCS_PREFIX: "/prod/",
    });

    expect(config.storage).toEqual({
      backend: "gcs",
      bucket: "buc-artifacts",
      prefix: "prod",
    });
  });
});

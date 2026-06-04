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
});

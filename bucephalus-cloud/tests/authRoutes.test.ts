import { describe, expect, test } from "bun:test";
import { handleAuthRoute } from "../src/routes/auth";
import type { AuthContext } from "../src/auth";
import type { ApiTokenRecord, ApiTokenRepository } from "../src/tokens/repository";

describe("auth token routes", () => {
  test("publishes safe login config without requiring user auth", async () => {
    const harness = tokenHarness();
    const response = await handleAuthRoute(
      new Request("https://cloud.example/v1/auth/config"),
      new URL("https://cloud.example/v1/auth/config"),
      harness.repository,
      null,
      {
        required: true,
        issuer: "https://accounts.google.com",
        audiences: ["client-1.apps.googleusercontent.com", "client-2.apps.googleusercontent.com"],
        cliClientId: "cli-client.apps.googleusercontent.com",
        cliScope: "openid email",
        jwksUrl: "https://www.googleapis.com/oauth2/v3/certs",
      },
    );

    expect(response!.status).toBe(200);
    const body = await response!.json();
    expect(body).toEqual({
      schema_version: "bucephalus_cloud_auth_config_v1",
      issuer: "https://accounts.google.com",
      client_id: "cli-client.apps.googleusercontent.com",
      audience: "cli-client.apps.googleusercontent.com",
      scope: "openid email",
    });
    expect(harness.created).toHaveLength(0);
  });

  test("exchanges an OAuth credential for a session token returned exactly once", async () => {
    const harness = tokenHarness();
    const response = await handleAuthRoute(
      new Request("https://cloud.example/v1/auth/sessions", { method: "POST" }),
      new URL("https://cloud.example/v1/auth/sessions"),
      harness.repository,
      oauthContext("user-a"),
    );

    expect(response!.status).toBe(201);
    const body = await response!.json();
    expect(body.token).toMatch(/^buc_/);
    expect(body.kind).toBe("session");
    expect(body.expires_at).toBeTruthy();
    expect(harness.created[0]).toMatchObject({
      kind: "session",
      issuer: "https://accounts.google.com",
      subject: "user-a",
      ttlSeconds: 30 * 24 * 60 * 60,
    });
  });

  test("rejects session creation from a Bucephalus token credential", async () => {
    const harness = tokenHarness();
    await expect(handleAuthRoute(
      new Request("https://cloud.example/v1/auth/sessions", { method: "POST" }),
      new URL("https://cloud.example/v1/auth/sessions"),
      harness.repository,
      tokenContext("user-a", "session"),
    )).rejects.toThrow("identity-provider credential");
    expect(harness.created).toHaveLength(0);
  });

  test("creates labeled non-expiring api keys from any credential", async () => {
    const harness = tokenHarness();
    const response = await handleAuthRoute(
      new Request("https://cloud.example/v1/auth/api-keys", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ label: "ci deploys" }),
      }),
      new URL("https://cloud.example/v1/auth/api-keys"),
      harness.repository,
      tokenContext("user-a", "session"),
    );

    expect(response!.status).toBe(201);
    const body = await response!.json();
    expect(body.token).toMatch(/^buc_/);
    expect(body.kind).toBe("api_key");
    expect(body.label).toBe("ci deploys");
    expect(body.expires_at).toBeNull();
  });

  test("token listing exposes metadata but never secrets or hashes", async () => {
    const harness = tokenHarness({ existing: [record("api_key", "ci deploys")] });
    const response = await handleAuthRoute(
      new Request("https://cloud.example/v1/auth/tokens"),
      new URL("https://cloud.example/v1/auth/tokens"),
      harness.repository,
      tokenContext("user-a", "session"),
    );

    const body = await response!.json();
    expect(body.tokens).toHaveLength(1);
    expect(body.tokens[0].token_prefix).toBe("buc_abcd1234");
    expect(body.tokens[0].token).toBeUndefined();
    expect(JSON.stringify(body)).not.toContain("hash");
  });

  test("revokes a token by id and 404s on unknown ids", async () => {
    const harness = tokenHarness({ existing: [record("api_key", "ci deploys")] });
    const response = await handleAuthRoute(
      new Request("https://cloud.example/v1/auth/tokens/token-1", { method: "DELETE" }),
      new URL("https://cloud.example/v1/auth/tokens/token-1"),
      harness.repository,
      tokenContext("user-a", "session"),
    );
    expect((await response!.json()).revoked).toBe(true);

    await expect(handleAuthRoute(
      new Request("https://cloud.example/v1/auth/tokens/token-1", { method: "DELETE" }),
      new URL("https://cloud.example/v1/auth/tokens/token-1"),
      harness.repository,
      tokenContext("user-a", "session"),
    )).rejects.toThrow("Token not found");
  });

  test("sign-out revokes the presented session bearer", async () => {
    const harness = tokenHarness({ existing: [record("session", null)] });
    const response = await handleAuthRoute(
      new Request("https://cloud.example/v1/auth/sessions/current", {
        method: "DELETE",
        headers: { authorization: "Bearer buc_current-session" },
      }),
      new URL("https://cloud.example/v1/auth/sessions/current"),
      harness.repository,
      tokenContext("user-a", "session"),
    );
    expect(await response!.json()).toEqual({ revoked: true });
    expect(harness.revokedSecrets).toEqual(["buc_current-session"]);
  });

  test("sign-out with a non-token bearer is rejected", async () => {
    const harness = tokenHarness();
    await expect(handleAuthRoute(
      new Request("https://cloud.example/v1/auth/sessions/current", {
        method: "DELETE",
        headers: { authorization: "Bearer eyJhbGciOi.google.jwt" },
      }),
      new URL("https://cloud.example/v1/auth/sessions/current"),
      harness.repository,
      oauthContext("user-a"),
    )).rejects.toThrow("different credential");
  });
});

function tokenHarness(input: { existing?: ApiTokenRecord[] } = {}) {
  const created: Record<string, unknown>[] = [];
  const revokedSecrets: string[] = [];
  let tokens = input.existing ?? [];

  const repository = {
    async createToken(args: { kind: string; issuer: string; subject: string; label?: string | null; ttlSeconds?: number | null }) {
      created.push(args);
      return {
        record: {
          ...record(args.kind as ApiTokenRecord["kind"], args.label ?? null),
          ttl_seconds: args.ttlSeconds ?? null,
          expires_at: args.ttlSeconds ? "2026-07-11T00:00:00Z" : null,
        },
        secret: "buc_new-secret",
      };
    },
    async listTokens() {
      return tokens;
    },
    async revokeToken(_ownerKey: string, tokenId: string) {
      const match = tokens.find((token) => token.token_id === tokenId) ?? null;
      tokens = tokens.filter((token) => token.token_id !== tokenId);
      return match;
    },
    async revokeBySecret(secret: string) {
      revokedSecrets.push(secret);
      return tokens[0] ?? null;
    },
  };

  return { created, revokedSecrets, repository: repository as unknown as ApiTokenRepository };
}

function record(kind: ApiTokenRecord["kind"], label: string | null): ApiTokenRecord {
  return {
    token_id: "token-1",
    token_prefix: "buc_abcd1234",
    kind,
    owner_key: "https://accounts.google.com:user-a",
    issuer: "https://accounts.google.com",
    subject: "user-a",
    label,
    ttl_seconds: kind === "session" ? 2592000 : null,
    expires_at: kind === "session" ? "2026-07-11T00:00:00Z" : null,
    last_used_at: null,
    created_at: "2026-06-11T00:00:00Z",
  };
}

function oauthContext(subject: string): AuthContext {
  return {
    subject,
    issuer: "https://accounts.google.com",
    audience: "client-id",
    claims: { sub: subject, iss: "https://accounts.google.com" },
  };
}

function tokenContext(subject: string, kind: string): AuthContext {
  return {
    subject,
    issuer: "https://accounts.google.com",
    audience: "bucephalus-token",
    claims: { token_id: "token-0", token_kind: kind },
  };
}

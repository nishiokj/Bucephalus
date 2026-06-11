import { describe, expect, test } from "bun:test";
import { CloudAuthenticator, type AuthContext } from "../src/auth";
import type { OAuthVerifier } from "../src/auth";
import { generateTokenSecret, hashTokenSecret, type ApiTokenRecord, type ApiTokenRepository } from "../src/tokens/repository";

describe("token secrets", () => {
  test("secrets are buc_-prefixed, unique, and hash deterministically", () => {
    const first = generateTokenSecret();
    const second = generateTokenSecret();
    expect(first).toMatch(/^buc_[A-Za-z0-9_-]{43}$/);
    expect(first).not.toBe(second);
    expect(hashTokenSecret(first)).toBe(hashTokenSecret(first));
    expect(hashTokenSecret(first)).not.toBe(hashTokenSecret(second));
    expect(hashTokenSecret(first)).toMatch(/^[0-9a-f]{64}$/);
  });
});

describe("cloud authenticator", () => {
  test("buc_ bearers resolve through the token repository to the original owner", async () => {
    const tokens = {
      async verifyToken(secret: string) {
        expect(secret).toBe("buc_valid");
        return tokenRecord();
      },
    };
    const verifier = {
      async requireUser(): Promise<AuthContext> {
        throw new Error("OAuth verifier should not be consulted for buc_ tokens");
      },
    };

    const authenticator = new CloudAuthenticator(
      verifier as unknown as OAuthVerifier,
      tokens as unknown as ApiTokenRepository,
    );
    const context = await authenticator.requireUser(bearerRequest("buc_valid"), "test");
    expect(context.subject).toBe("user-a");
    expect(context.issuer).toBe("https://accounts.google.com");
    expect(context.claims.token_kind).toBe("session");
  });

  test("revoked or expired buc_ bearers are rejected without OAuth fallback", async () => {
    const tokens = {
      async verifyToken() {
        return null;
      },
    };
    const verifier = {
      async requireUser(): Promise<AuthContext> {
        throw new Error("OAuth verifier should not be consulted for buc_ tokens");
      },
    };

    const authenticator = new CloudAuthenticator(
      verifier as unknown as OAuthVerifier,
      tokens as unknown as ApiTokenRepository,
    );
    await expect(authenticator.requireUser(bearerRequest("buc_revoked"), "test"))
      .rejects.toThrow("invalid, expired, or revoked");
  });

  test("non-buc_ bearers delegate to the OAuth verifier", async () => {
    const tokens = {
      async verifyToken(): Promise<ApiTokenRecord | null> {
        throw new Error("Token repository should not be consulted for OAuth bearers");
      },
    };
    const verifier = {
      async requireUser(): Promise<AuthContext> {
        return {
          subject: "user-a",
          issuer: "https://accounts.google.com",
          audience: "client-id",
          claims: {},
        };
      },
    };

    const authenticator = new CloudAuthenticator(
      verifier as unknown as OAuthVerifier,
      tokens as unknown as ApiTokenRepository,
    );
    const context = await authenticator.requireUser(bearerRequest("eyJhbGciOi.fake.jwt"), "test");
    expect(context.subject).toBe("user-a");
    expect(context.claims.token_kind).toBeUndefined();
  });
});

function bearerRequest(token: string): Request {
  return new Request("https://cloud.example/v1/runs", {
    headers: { authorization: `Bearer ${token}` },
  });
}

function tokenRecord(): ApiTokenRecord {
  return {
    token_id: "token-1",
    token_prefix: "buc_valid".slice(0, 12),
    kind: "session",
    owner_key: "https://accounts.google.com:user-a",
    issuer: "https://accounts.google.com",
    subject: "user-a",
    label: null,
    ttl_seconds: 2592000,
    expires_at: "2026-07-11T00:00:00Z",
    last_used_at: null,
    created_at: "2026-06-11T00:00:00Z",
  };
}

import { describe, expect, test } from "bun:test";
import { bearerToken, OAuthVerifier } from "../src/auth";
import { HttpError } from "../src/http";

describe("OAuth verifier", () => {
  test("rejects missing user authentication when required", async () => {
    const verifier = new OAuthVerifier({
      required: true,
      issuer: "https://issuer.example",
      audience: "bucephalus-cloud",
      jwksUrl: "https://issuer.example/.well-known/jwks.json",
    });

    await expect(verifier.requireUser(new Request("http://localhost/v1/imports"), "Cloud API"))
      .rejects
      .toThrow(HttpError);
  });

  test("normalizes malformed JWT syntax into an unauthorized HTTP error", async () => {
    const verifier = new OAuthVerifier({
      required: true,
      issuer: "https://issuer.example",
      audience: "bucephalus-cloud",
      jwksUrl: "https://issuer.example/.well-known/jwks.json",
    });

    await expect(verifier.verifyToken("not-json.not-json.not-json"))
      .rejects
      .toMatchObject({
        status: 401,
        code: "unauthorized",
      });
  });

  test("verifies RS256 JWTs against JWKS and caches signing keys", async () => {
    const { privateKey, publicJwk } = await rsaSigningKey("kid-1");
    const originalFetch = globalThis.fetch;
    let fetchCount = 0;
    globalThis.fetch = (async () => {
      fetchCount += 1;
      return Response.json({ keys: [publicJwk] });
    }) as unknown as typeof fetch;
    try {
      const verifier = new OAuthVerifier({
        required: true,
        issuer: "https://issuer.example",
        audience: "bucephalus-cloud",
        jwksUrl: "https://issuer.example/.well-known/jwks.json",
      });
      const token = await signJwt(privateKey, {
        kid: "kid-1",
        payload: {
          iss: "https://issuer.example",
          aud: ["other-audience", "bucephalus-cloud"],
          sub: "user-123",
          exp: Math.floor(Date.now() / 1000) + 300,
        },
      });

      await expect(verifier.verifyToken(token)).resolves.toMatchObject({
        subject: "user-123",
        issuer: "https://issuer.example",
      });
      await expect(verifier.verifyToken(token)).resolves.toMatchObject({
        subject: "user-123",
      });
      expect(fetchCount).toBe(1);
    } finally {
      globalThis.fetch = originalFetch;
    }
  });

  test("only extracts exact bearer authorization headers", () => {
    expect(bearerToken(new Request("http://localhost", {
      headers: { authorization: "Bearer token-123" },
    }))).toBe("token-123");
    expect(bearerToken(new Request("http://localhost", {
      headers: { authorization: "bearer token-123" },
    }))).toBeNull();
    expect(bearerToken(new Request("http://localhost", {
      headers: { authorization: "Bearer" },
    }))).toBeNull();
  });

  test("rejects unauthenticated Cloud API mode", () => {
    expect(() => new OAuthVerifier({
      required: false,
      issuer: null,
      audience: null,
      jwksUrl: null,
    })).toThrow("Unauthenticated Bucephalus Cloud API mode is not supported");
  });
});

async function rsaSigningKey(
  kid: string,
): Promise<{ privateKey: CryptoKey; publicJwk: JsonWebKey & { kid: string; alg: string; use: string } }> {
  const keyPair = await crypto.subtle.generateKey(
    {
      name: "RSASSA-PKCS1-v1_5",
      modulusLength: 2048,
      publicExponent: new Uint8Array([1, 0, 1]),
      hash: "SHA-256",
    },
    true,
    ["sign", "verify"],
  );
  const publicJwk = await crypto.subtle.exportKey("jwk", keyPair.publicKey);
  return {
    privateKey: keyPair.privateKey,
    publicJwk: {
      ...publicJwk,
      kid,
      alg: "RS256",
      use: "sig",
    },
  };
}

async function signJwt(
  privateKey: CryptoKey,
  input: { kid: string; payload: Record<string, unknown> },
): Promise<string> {
  const encodedHeader = base64UrlJson({
    alg: "RS256",
    typ: "JWT",
    kid: input.kid,
  });
  const encodedPayload = base64UrlJson(input.payload);
  const signingInput = `${encodedHeader}.${encodedPayload}`;
  const signature = await crypto.subtle.sign(
    "RSASSA-PKCS1-v1_5",
    privateKey,
    new TextEncoder().encode(signingInput),
  );
  return `${signingInput}.${base64UrlBytes(new Uint8Array(signature))}`;
}

function base64UrlJson(value: unknown): string {
  return base64UrlBytes(new TextEncoder().encode(JSON.stringify(value)));
}

function base64UrlBytes(value: Uint8Array): string {
  return Buffer.from(value)
    .toString("base64")
    .replaceAll("+", "-")
    .replaceAll("/", "_")
    .replace(/=+$/, "");
}

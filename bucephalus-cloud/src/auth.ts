import type { AuthConfig } from "./config";
import { HttpError, isRecord } from "./http";

export interface AuthContext {
  subject: string;
  issuer: string;
  audience: string | string[];
  claims: Record<string, unknown>;
}

export function authOwnerKey(auth: AuthContext | null | undefined): string | undefined {
  if (!auth) {
    return undefined;
  }
  return `${auth.issuer}:${auth.subject}`;
}

interface Jwks {
  keys: JsonWebKey[];
}

const JWKS_CACHE_MS = 5 * 60 * 1000;

export class OAuthVerifier {
  private jwks: { fetchedAt: number; keys: JsonWebKey[] } | null = null;

  constructor(private readonly config: AuthConfig) {
    if (!config.required) {
      throw new Error("Unauthenticated Bucephalus Cloud API mode is not supported");
    }
    if (!config.issuer || !config.audiences || !config.jwksUrl) {
      throw new Error(
        "OAuth is required; set BUCEPHALUS_CLOUD_OAUTH_ISSUER, BUCEPHALUS_CLOUD_OAUTH_AUDIENCE, and BUCEPHALUS_CLOUD_OAUTH_JWKS_URL",
      );
    }
  }

  async requireUser(request: Request, scope: string): Promise<AuthContext> {
    const token = bearerToken(request);
    if (!token) {
      throw new HttpError(401, "unauthorized", `${scope} requires OAuth bearer authentication`);
    }
    return await this.verifyToken(token, scope);
  }

  async verifyToken(token: string, scope = "Cloud API"): Promise<AuthContext> {
    const jwt = parseJwt(token);
    if (jwt.header.alg !== "RS256") {
      throw new HttpError(401, "unauthorized", `${scope} token must use RS256`);
    }
    if (!this.config.issuer || !this.config.audiences || !this.config.jwksUrl) {
      throw new HttpError(401, "unauthorized", `${scope} OAuth verifier is not configured`);
    }
    validateClaims(jwt.payload, {
      issuer: this.config.issuer,
      audiences: this.config.audiences,
      scope,
    });

    const key = await this.keyFor(jwt.header);
    const valid = await crypto.subtle.verify(
      "RSASSA-PKCS1-v1_5",
      key,
      arrayBuffer(jwt.signature),
      new TextEncoder().encode(`${jwt.encodedHeader}.${jwt.encodedPayload}`),
    );
    if (!valid) {
      throw new HttpError(401, "unauthorized", `${scope} token signature is invalid`);
    }

    return {
      subject: String(jwt.payload.sub),
      issuer: String(jwt.payload.iss),
      audience: jwt.payload.aud as string | string[],
      claims: jwt.payload,
    };
  }

  private async keyFor(header: Record<string, unknown>): Promise<CryptoKey> {
    const kid = typeof header.kid === "string" ? header.kid : null;
    const keys = await this.fetchKeys();
    const jwk = keys.find((candidate) => {
      const keyed = candidate as JsonWebKey & { kid?: string };
      return (!kid || keyed.kid === kid) && candidate.kty === "RSA";
    });
    if (!jwk) {
      throw new HttpError(401, "unauthorized", "OAuth signing key not found");
    }
    return await crypto.subtle.importKey(
      "jwk",
      jwk,
      {
        name: "RSASSA-PKCS1-v1_5",
        hash: "SHA-256",
      },
      false,
      ["verify"],
    );
  }

  private async fetchKeys(): Promise<JsonWebKey[]> {
    const now = Date.now();
    if (this.jwks && now - this.jwks.fetchedAt < JWKS_CACHE_MS) {
      return this.jwks.keys;
    }
    if (!this.config.jwksUrl) {
      throw new HttpError(401, "unauthorized", "OAuth JWKS URL is not configured");
    }
    const response = await fetch(this.config.jwksUrl);
    if (!response.ok) {
      throw new HttpError(503, "oauth_jwks_unavailable", "OAuth JWKS endpoint is unavailable");
    }
    const body = await response.json();
    if (!isRecord(body) || !Array.isArray(body.keys)) {
      throw new HttpError(503, "oauth_jwks_invalid", "OAuth JWKS endpoint returned an invalid document");
    }
    const keys = (body as unknown as Jwks).keys;
    this.jwks = { fetchedAt: now, keys };
    return keys;
  }
}

export function bearerToken(request: Request): string | null {
  const authorization = request.headers.get("authorization");
  return authorization?.startsWith("Bearer ") ? authorization.slice("Bearer ".length) : null;
}

function parseJwt(token: string): {
  header: Record<string, unknown>;
  payload: Record<string, unknown>;
  signature: Uint8Array;
  encodedHeader: string;
  encodedPayload: string;
} {
  const parts = token.split(".");
  if (parts.length !== 3) {
    throw new HttpError(401, "unauthorized", "OAuth bearer token must be a JWT");
  }
  const [encodedHeader, encodedPayload, encodedSignature] = parts as [string, string, string];
  let header: unknown;
  let payload: unknown;
  let signature: Uint8Array;
  try {
    header = jsonFromBase64Url(encodedHeader);
    payload = jsonFromBase64Url(encodedPayload);
    signature = bytesFromBase64Url(encodedSignature);
  } catch {
    throw new HttpError(401, "unauthorized", "OAuth bearer token has invalid encoding");
  }
  if (!isRecord(header) || !isRecord(payload)) {
    throw new HttpError(401, "unauthorized", "OAuth bearer token has invalid JSON");
  }
  return {
    header,
    payload,
    signature,
    encodedHeader,
    encodedPayload,
  };
}

function validateClaims(
  claims: Record<string, unknown>,
  expected: { issuer: string; audiences: string[]; scope: string },
): void {
  if (claims.iss !== expected.issuer) {
    throw new HttpError(401, "unauthorized", `${expected.scope} token issuer is invalid`);
  }
  if (typeof claims.sub !== "string" || claims.sub.length === 0) {
    throw new HttpError(401, "unauthorized", `${expected.scope} token subject is missing`);
  }
  const audience = claims.aud;
  const audienceMatches = expected.audiences.some((expectedAudience) =>
    audience === expectedAudience
    || (Array.isArray(audience) && audience.includes(expectedAudience))
  );
  if (!audienceMatches) {
    throw new HttpError(401, "unauthorized", `${expected.scope} token audience is invalid`);
  }
  const now = Math.floor(Date.now() / 1000);
  if (typeof claims.exp !== "number" || claims.exp <= now) {
    throw new HttpError(401, "unauthorized", `${expected.scope} token is expired`);
  }
  if (typeof claims.nbf === "number" && claims.nbf > now + 60) {
    throw new HttpError(401, "unauthorized", `${expected.scope} token is not valid yet`);
  }
}

function jsonFromBase64Url(value: string): unknown {
  return JSON.parse(new TextDecoder().decode(bytesFromBase64Url(value)));
}

function bytesFromBase64Url(value: string): Uint8Array {
  const normalized = value.replaceAll("-", "+").replaceAll("_", "/");
  const padded = normalized.padEnd(normalized.length + ((4 - (normalized.length % 4)) % 4), "=");
  return Uint8Array.from(Buffer.from(padded, "base64"));
}

function arrayBuffer(bytes: Uint8Array): ArrayBuffer {
  return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer;
}

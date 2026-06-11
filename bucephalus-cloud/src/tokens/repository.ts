import { createHash, randomBytes } from "node:crypto";
import type { Sql } from "../db/client";

export const TOKEN_SECRET_PREFIX = "buc_";

export type ApiTokenKind = "session" | "api_key";

export interface ApiTokenRecord {
  token_id: string;
  token_prefix: string;
  kind: ApiTokenKind;
  owner_key: string;
  issuer: string;
  subject: string;
  label: string | null;
  ttl_seconds: number | null;
  expires_at: string | null;
  last_used_at: string | null;
  created_at: string;
}

export function generateTokenSecret(): string {
  return `${TOKEN_SECRET_PREFIX}${randomBytes(32).toString("base64url")}`;
}

export function hashTokenSecret(secret: string): string {
  return createHash("sha256").update(secret).digest("hex");
}

// Tokens are opaque and hashed at rest: the secret is returned exactly once at
// creation, and verification is a hash lookup, so a database read can never
// recover a usable credential.
export class ApiTokenRepository {
  constructor(private readonly sql: Sql) {}

  async createToken(input: {
    kind: ApiTokenKind;
    issuer: string;
    subject: string;
    label?: string | null;
    ttlSeconds?: number | null;
  }): Promise<{ record: ApiTokenRecord; secret: string }> {
    const secret = generateTokenSecret();
    const ttlSeconds = input.ttlSeconds ?? null;
    const rows = await this.sql`
      insert into cloud.api_tokens (
        token_hash,
        token_prefix,
        kind,
        owner_key,
        issuer,
        subject,
        label,
        ttl_seconds,
        expires_at
      )
      values (
        ${hashTokenSecret(secret)},
        ${secret.slice(0, TOKEN_SECRET_PREFIX.length + 8)},
        ${input.kind},
        ${`${input.issuer}:${input.subject}`},
        ${input.issuer},
        ${input.subject},
        ${input.label ?? null},
        ${ttlSeconds},
        ${ttlSeconds === null ? null : this.sql`now() + make_interval(secs => ${ttlSeconds})`}
      )
      returning ${this.metadataColumns()}
    `;
    return { record: rows[0] as ApiTokenRecord, secret };
  }

  // Sessions slide: each successful verification pushes expires_at out by the
  // token's ttl, so an active user is never signed out mid-work while an
  // abandoned session still dies after one ttl of silence.
  async verifyToken(secret: string): Promise<ApiTokenRecord | null> {
    const rows = await this.sql`
      update cloud.api_tokens
      set last_used_at = now(),
          expires_at = case
            when ttl_seconds is null then expires_at
            else now() + make_interval(secs => ttl_seconds)
          end
      where token_hash = ${hashTokenSecret(secret)}
        and (expires_at is null or expires_at > now())
      returning ${this.metadataColumns()}
    `;
    return (rows[0] as ApiTokenRecord | undefined) ?? null;
  }

  async listTokens(ownerKey: string): Promise<ApiTokenRecord[]> {
    await this.sql`
      delete from cloud.api_tokens
      where owner_key = ${ownerKey} and expires_at is not null and expires_at <= now()
    `;
    const rows = await this.sql`
      select ${this.metadataColumns()}
      from cloud.api_tokens
      where owner_key = ${ownerKey}
      order by created_at desc
    `;
    return rows as unknown as ApiTokenRecord[];
  }

  async revokeToken(ownerKey: string, tokenId: string): Promise<ApiTokenRecord | null> {
    const rows = await this.sql`
      delete from cloud.api_tokens
      where owner_key = ${ownerKey} and token_id = ${tokenId}
      returning ${this.metadataColumns()}
    `;
    return (rows[0] as ApiTokenRecord | undefined) ?? null;
  }

  async revokeBySecret(secret: string): Promise<ApiTokenRecord | null> {
    const rows = await this.sql`
      delete from cloud.api_tokens
      where token_hash = ${hashTokenSecret(secret)}
      returning ${this.metadataColumns()}
    `;
    return (rows[0] as ApiTokenRecord | undefined) ?? null;
  }

  private metadataColumns() {
    return this.sql`token_id, token_prefix, kind, owner_key, issuer, subject, label, ttl_seconds, expires_at, last_used_at, created_at`;
  }
}

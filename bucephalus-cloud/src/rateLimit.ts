import { createHash } from "node:crypto";
import type { RateLimitConfig } from "./config";
import { jsonResponse } from "./http";

type BucketKind = "ip" | "credential";

interface BucketState {
  windowStartedAt: number;
  count: number;
}

interface BucketDecision {
  kind: BucketKind;
  limited: boolean;
  limit: number;
  remaining: number;
  resetAt: number;
}

export interface RateLimitDecision {
  allowed: boolean;
  bucket: BucketDecision;
}

export class InMemoryRateLimiter {
  private readonly buckets = new Map<string, BucketState>();
  private lastCleanupAt = 0;

  constructor(private readonly config: RateLimitConfig) {}

  check(request: Request, url: URL, now = Date.now()): RateLimitDecision | null {
    if (!this.config.enabled || !isRateLimitedRequest(request, url)) {
      return null;
    }

    this.cleanup(now);

    const decisions = [
      this.consume("ip", clientIpKey(request), this.config.ipMax, now),
      ...credentialKey(request).map((key) =>
        this.consume("credential", key, this.config.credentialMax, now)
      ),
    ];
    const limited = decisions.find((decision) => decision.limited);
    if (limited) {
      return { allowed: false, bucket: limited };
    }
    return {
      allowed: true,
      bucket: mostConstrained(decisions),
    };
  }

  private consume(kind: BucketKind, key: string, limit: number, now: number): BucketDecision {
    const bucketKey = `${kind}:${key}`;
    const existing = this.buckets.get(bucketKey);
    const state = existing && now < existing.windowStartedAt + this.config.windowMs
      ? existing
      : { windowStartedAt: now, count: 0 };
    if (state.count >= limit) {
      this.buckets.set(bucketKey, state);
      return {
        kind,
        limited: true,
        limit,
        remaining: 0,
        resetAt: state.windowStartedAt + this.config.windowMs,
      };
    }
    state.count += 1;
    this.buckets.set(bucketKey, state);
    return {
      kind,
      limited: false,
      limit,
      remaining: Math.max(0, limit - state.count),
      resetAt: state.windowStartedAt + this.config.windowMs,
    };
  }

  private cleanup(now: number): void {
    if (now - this.lastCleanupAt < this.config.windowMs) {
      return;
    }
    this.lastCleanupAt = now;
    for (const [key, state] of this.buckets) {
      if (now >= state.windowStartedAt + this.config.windowMs) {
        this.buckets.delete(key);
      }
    }
  }
}

export function rateLimitResponse(decision: RateLimitDecision, now = Date.now()): Response {
  const retryAfterSeconds = Math.max(1, Math.ceil((decision.bucket.resetAt - now) / 1000));
  return jsonResponse(
    {
      code: "rate_limited",
      message: "Too many requests; retry after the rate limit window resets",
      detail: {
        bucket: decision.bucket.kind,
        retry_after_seconds: retryAfterSeconds,
      },
    },
    {
      status: 429,
      headers: rateLimitHeaders(decision.bucket, now),
    },
  );
}

function rateLimitHeaders(bucket: BucketDecision, now: number): HeadersInit {
  const resetSeconds = Math.max(1, Math.ceil((bucket.resetAt - now) / 1000));
  return {
    "retry-after": String(resetSeconds),
    "ratelimit-limit": String(bucket.limit),
    "ratelimit-remaining": String(bucket.remaining),
    "ratelimit-reset": String(resetSeconds),
  };
}

function isRateLimitedRequest(request: Request, url: URL): boolean {
  return request.method !== "OPTIONS" && url.pathname.startsWith("/v1/");
}

function mostConstrained(decisions: BucketDecision[]): BucketDecision {
  return decisions.reduce((current, candidate) =>
    candidate.remaining < current.remaining ? candidate : current
  );
}

function clientIpKey(request: Request): string {
  const forwardedFor = request.headers.get("x-forwarded-for");
  const firstForwarded = forwardedFor?.split(",")[0]?.trim();
  const ip = firstForwarded
    || request.headers.get("x-real-ip")?.trim()
    || request.headers.get("cf-connecting-ip")?.trim()
    || "unknown";
  return stableHash(ip);
}

function credentialKey(request: Request): string[] {
  const authorization = request.headers.get("authorization");
  const bearer = authorization?.startsWith("Bearer ") ? authorization.slice("Bearer ".length).trim() : "";
  const workerToken = request.headers.get("x-bucephalus-worker-token")?.trim() ?? "";
  const runnerAdminToken = request.headers.get("x-bucephalus-runner-admin-token")?.trim() ?? "";
  const credential = bearer || workerToken || runnerAdminToken;
  return credential.length > 0 ? [stableHash(credential)] : [];
}

function stableHash(value: string): string {
  return createHash("sha256").update(value).digest("hex");
}

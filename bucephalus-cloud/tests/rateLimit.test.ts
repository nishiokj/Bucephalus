import { describe, expect, test } from "bun:test";
import type { RateLimitConfig } from "../src/config";
import { InMemoryRateLimiter, rateLimitResponse } from "../src/rateLimit";

describe("API rate limiting", () => {
  test("limits unauthenticated API requests by client IP", async () => {
    const limiter = testLimiter({ ipMax: 2, credentialMax: 100 });
    const request = apiRequest({ "x-forwarded-for": "203.0.113.10" });
    const url = new URL(request.url);

    expect(limiter.check(request, url, 1_000)?.allowed).toBe(true);
    expect(limiter.check(request, url, 1_001)?.allowed).toBe(true);
    const decision = limiter.check(request, url, 1_002);

    expect(decision?.allowed).toBe(false);
    expect(decision?.bucket.kind).toBe("ip");

    const response = rateLimitResponse(decision!, 1_002);
    const body = await response.json();
    expect(response.status).toBe(429);
    expect(response.headers.get("retry-after")).toBe("60");
    expect(response.headers.get("ratelimit-limit")).toBe("2");
    expect(response.headers.get("ratelimit-remaining")).toBe("0");
    expect(body).toMatchObject({
      code: "rate_limited",
      detail: { bucket: "ip" },
    });
  });

  test("resets buckets after the configured window", () => {
    const limiter = testLimiter({ windowMs: 1_000, ipMax: 1 });
    const request = apiRequest({ "x-forwarded-for": "203.0.113.11" });
    const url = new URL(request.url);

    expect(limiter.check(request, url, 10_000)?.allowed).toBe(true);
    expect(limiter.check(request, url, 10_500)?.allowed).toBe(false);
    expect(limiter.check(request, url, 11_000)?.allowed).toBe(true);
  });

  test("limits the same credential across different client IPs", () => {
    const limiter = testLimiter({ ipMax: 100, credentialMax: 2 });
    const first = apiRequest({
      authorization: "Bearer buc_same-token",
      "x-forwarded-for": "203.0.113.12",
    });
    const second = apiRequest({
      authorization: "Bearer buc_same-token",
      "x-forwarded-for": "203.0.113.13",
    });
    const third = apiRequest({
      authorization: "Bearer buc_same-token",
      "x-forwarded-for": "203.0.113.14",
    });

    expect(limiter.check(first, new URL(first.url), 1_000)?.allowed).toBe(true);
    expect(limiter.check(second, new URL(second.url), 1_001)?.allowed).toBe(true);
    const decision = limiter.check(third, new URL(third.url), 1_002);

    expect(decision?.allowed).toBe(false);
    expect(decision?.bucket.kind).toBe("credential");
  });

  test("still limits one IP when callers rotate bearer values", () => {
    const limiter = testLimiter({ ipMax: 2, credentialMax: 100 });

    for (const token of ["buc_one", "buc_two"]) {
      const request = apiRequest({
        authorization: `Bearer ${token}`,
        "x-forwarded-for": "203.0.113.15",
      });
      expect(limiter.check(request, new URL(request.url), 1_000)?.allowed).toBe(true);
    }

    const rotated = apiRequest({
      authorization: "Bearer buc_three",
      "x-forwarded-for": "203.0.113.15",
    });
    const decision = limiter.check(rotated, new URL(rotated.url), 1_002);
    expect(decision?.allowed).toBe(false);
    expect(decision?.bucket.kind).toBe("ip");
  });

  test("skips health checks and CORS preflights", () => {
    const limiter = testLimiter({ ipMax: 1 });
    const preflight = new Request("https://cloud.example/v1/runs", { method: "OPTIONS" });
    const health = new Request("https://cloud.example/healthz");

    expect(limiter.check(preflight, new URL(preflight.url), 1_000)).toBeNull();
    expect(limiter.check(health, new URL(health.url), 1_000)).toBeNull();
  });
});

function apiRequest(headers: HeadersInit = {}): Request {
  return new Request("https://cloud.example/v1/runs", { headers });
}

function testLimiter(overrides: Partial<RateLimitConfig> = {}): InMemoryRateLimiter {
  return new InMemoryRateLimiter({
    enabled: true,
    windowMs: 60_000,
    ipMax: 300,
    credentialMax: 120,
    ...overrides,
  });
}

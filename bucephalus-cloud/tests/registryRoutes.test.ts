import { describe, expect, test } from "bun:test";
import { handleRegistryRoute } from "../src/routes/registry";
import type { RegistryRepository } from "../src/registry/repository";

describe("registry routes", () => {
  const validDigest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

  test("registry search without q lists inventory", async () => {
    const observed: { q?: string; limit?: number } = {};
    const repository = {
      async search(options: { q: string; limit: number }) {
        observed.q = options.q;
        observed.limit = options.limit;
        return [
          {
            kind: "variant",
            content_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            display_name: "Baseline",
            aliases: ["baseline"],
            score: 0,
            metadata: {},
            used_by_runs: 3,
            last_used_at: "2026-06-12T14:30:00.000Z",
          },
          {
            kind: "metric",
            content_digest: "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            display_name: "Accuracy",
            aliases: [],
            score: 0,
            metadata: {},
            used_by_runs: 0,
            last_used_at: null,
          },
        ];
      },
    };

    const response = await handleRegistryRoute(
      new Request("https://cloud.example/v1/registry/search?limit=100"),
      new URL("https://cloud.example/v1/registry/search?limit=100"),
      repository as unknown as RegistryRepository,
    );

    expect(response).not.toBeNull();
    expect(response!.status).toBe(200);
    expect(observed).toEqual({ q: "", limit: 100 });
    const body = await response!.json();
    expect(body.hits).toEqual([
      expect.objectContaining({
        content_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        used_by_runs: 3,
        last_used_at: "2026-06-12T14:30:00.000Z",
      }),
      expect.objectContaining({
        content_digest: "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        used_by_runs: 0,
        last_used_at: null,
      }),
    ]);
  });

  test("registry search trims blank q into inventory search", async () => {
    const observed: { q?: string } = {};
    const repository = {
      async search(options: { q: string }) {
        observed.q = options.q;
        return [];
      },
    };

    const response = await handleRegistryRoute(
      new Request("https://cloud.example/v1/registry/search?q=%20%20&limit=1"),
      new URL("https://cloud.example/v1/registry/search?q=%20%20&limit=1"),
      repository as unknown as RegistryRepository,
    );

    expect(response).not.toBeNull();
    expect(response!.status).toBe(200);
    expect(observed.q).toBe("");
  });

  test("registry search validates kind before querying", async () => {
    const observed: { kind: string | undefined } = { kind: undefined };
    const repository = {
      async search(options: { kind?: string }) {
        observed.kind = options.kind;
        return [];
      },
    };

    const response = await handleRegistryRoute(
      new Request("https://cloud.example/v1/registry/search?kind=variant&limit=1"),
      new URL("https://cloud.example/v1/registry/search?kind=variant&limit=1"),
      repository as unknown as RegistryRepository,
    );

    expect(response).not.toBeNull();
    expect(response!.status).toBe(200);
    expect(observed.kind).toBe("variant");

    await expect(handleRegistryRoute(
      new Request("https://cloud.example/v1/registry/search?kind=agent"),
      new URL("https://cloud.example/v1/registry/search?kind=agent"),
      repository as unknown as RegistryRepository,
    )).rejects.toThrow("/kind must be a valid entity kind");
  });

  test("registry search rejects invalid limits before querying", async () => {
    const repository = {
      async search() {
        throw new Error("search should not be called");
      },
    };

    await expect(handleRegistryRoute(
      new Request("https://cloud.example/v1/registry/search?limit=potato"),
      new URL("https://cloud.example/v1/registry/search?limit=potato"),
      repository as unknown as RegistryRepository,
    )).rejects.toThrow("/limit must be an integer");

    await expect(handleRegistryRoute(
      new Request("https://cloud.example/v1/registry/search?limit=201"),
      new URL("https://cloud.example/v1/registry/search?limit=201"),
      repository as unknown as RegistryRepository,
    )).rejects.toThrow("/limit must be <= 200");
  });

  test("registry object lookup validates digest before querying", async () => {
    const repository = {
      async getContentObject() {
        throw new Error("getContentObject should not be called");
      },
      async aliasesForDigest() {
        throw new Error("aliasesForDigest should not be called");
      },
    };

    await expect(handleRegistryRoute(
      new Request("https://cloud.example/v1/registry/objects/sha256%3ANOTHEX"),
      new URL("https://cloud.example/v1/registry/objects/sha256%3ANOTHEX"),
      repository as unknown as RegistryRepository,
    )).rejects.toThrow("/digest must be sha256:<64 lowercase hex chars>");
  });

  test("registry object lookup includes usage for runs by owner", async () => {
    const observed: { digest?: string; usageOwnerKey: string | undefined } = { usageOwnerKey: undefined };
    const repository = {
      async getContentObject(digest: string) {
        observed.digest = digest;
        return {
          content_digest: digest,
          kind: "variant",
          schema_version: "v1",
          canonical_json: { display_name: "Baseline" },
          canonical_size_bytes: 28,
          created_at: "2026-06-01T00:00:00.000Z",
          created_by: null,
          source_uri: null,
        };
      },
      async aliasesForDigest() {
        return [];
      },
      async usageForDigest(_digest: string, ownerKey?: string) {
        observed.usageOwnerKey = ownerKey;
        return {
          used_by_runs: 2,
          last_used_at: "2026-06-12T14:30:00.000Z",
        };
      },
    };

    const response = await handleRegistryRoute(
      new Request(`https://cloud.example/v1/registry/objects/${encodeURIComponent(validDigest)}`),
      new URL(`https://cloud.example/v1/registry/objects/${encodeURIComponent(validDigest)}`),
      repository as unknown as RegistryRepository,
      {
        issuer: "https://issuer.example",
        subject: "user-1",
        audience: "cloud",
        claims: {},
      },
    );

    expect(response).not.toBeNull();
    expect(response!.status).toBe(200);
    expect(observed).toEqual({
      digest: validDigest,
      usageOwnerKey: "https://issuer.example:user-1",
    });
    const body = await response!.json();
    expect(body.used_by_runs).toBe(2);
    expect(body.last_used_at).toBe("2026-06-12T14:30:00.000Z");
  });

  test("registry object lookup includes empty usage for resources never used by runs", async () => {
    const repository = {
      async getContentObject(digest: string) {
        return {
          content_digest: digest,
          kind: "metric",
          schema_version: "v1",
          canonical_json: { display_name: "Accuracy" },
          canonical_size_bytes: 28,
          created_at: "2026-06-01T00:00:00.000Z",
          created_by: null,
          source_uri: null,
        };
      },
      async aliasesForDigest() {
        return [];
      },
      async usageForDigest() {
        return {
          used_by_runs: 0,
          last_used_at: null,
        };
      },
    };

    const response = await handleRegistryRoute(
      new Request(`https://cloud.example/v1/registry/objects/${encodeURIComponent(validDigest)}`),
      new URL(`https://cloud.example/v1/registry/objects/${encodeURIComponent(validDigest)}`),
      repository as unknown as RegistryRepository,
    );

    expect(response).not.toBeNull();
    expect(response!.status).toBe(200);
    const body = await response!.json();
    expect(body.used_by_runs).toBe(0);
    expect(body.last_used_at).toBeNull();
  });

  test("registry mutation routes reject invalid kinds before repository calls", async () => {
    const repository = {
      async getContentObject() {
        throw new Error("getContentObject should not be called");
      },
      async reviewAliases() {
        throw new Error("reviewAliases should not be called");
      },
      async register() {
        throw new Error("register should not be called");
      },
      async createAlias() {
        throw new Error("createAlias should not be called");
      },
    };

    await expect(handleRegistryRoute(
      jsonRequest("/v1/registry/canonicalize", { kind: "agent", object: {} }),
      new URL("https://cloud.example/v1/registry/canonicalize"),
      repository as unknown as RegistryRepository,
    )).rejects.toThrow("/kind must be a valid entity kind");

    await expect(handleRegistryRoute(
      jsonRequest("/v1/registry/review", { kind: "agent", content_digest: validDigest }),
      new URL("https://cloud.example/v1/registry/review"),
      repository as unknown as RegistryRepository,
    )).rejects.toThrow("/kind must be a valid entity kind");

    await expect(handleRegistryRoute(
      jsonRequest("/v1/registry/objects", {
        kind: "agent",
        schema_version: "v1",
        canonical_json: {},
      }),
      new URL("https://cloud.example/v1/registry/objects"),
      repository as unknown as RegistryRepository,
    )).rejects.toThrow("/kind must be a valid entity kind");

    await expect(handleRegistryRoute(
      jsonRequest("/v1/registry/resolve", { ref: { kind: "agent", digest: validDigest } }),
      new URL("https://cloud.example/v1/registry/resolve"),
      repository as unknown as RegistryRepository,
    )).rejects.toThrow("/ref/kind must be a valid entity kind");

    await expect(handleRegistryRoute(
      jsonRequest("/v1/registry/aliases", {
        kind: "agent",
        alias: "codex",
        content_digest: validDigest,
      }),
      new URL("https://cloud.example/v1/registry/aliases"),
      repository as unknown as RegistryRepository,
    )).rejects.toThrow("/kind must be a valid entity kind");
  });

  test("registry routes reject malformed digests before repository calls", async () => {
    const repository = {
      async getContentObject() {
        throw new Error("getContentObject should not be called");
      },
      async register() {
        throw new Error("register should not be called");
      },
      async hasDigest() {
        throw new Error("hasDigest should not be called");
      },
      async createAlias() {
        throw new Error("createAlias should not be called");
      },
    };

    await expect(handleRegistryRoute(
      jsonRequest("/v1/registry/review", {
        kind: "variant",
        content_digest: "sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
      }),
      new URL("https://cloud.example/v1/registry/review"),
      repository as unknown as RegistryRepository,
    )).rejects.toThrow("/content_digest must be sha256:<64 lowercase hex chars>");

    await expect(handleRegistryRoute(
      jsonRequest("/v1/registry/objects", {
        kind: "variant",
        schema_version: "v1",
        canonical_json: {},
        expected_digest: "sha256:short",
      }),
      new URL("https://cloud.example/v1/registry/objects"),
      repository as unknown as RegistryRepository,
    )).rejects.toThrow("/expected_digest must be sha256:<64 lowercase hex chars>");

    await expect(handleRegistryRoute(
      jsonRequest("/v1/registry/resolve", {
        ref: {
          kind: "variant",
          digest: "sha256:short",
        },
      }),
      new URL("https://cloud.example/v1/registry/resolve"),
      repository as unknown as RegistryRepository,
    )).rejects.toThrow("/ref/digest must be sha256:<64 lowercase hex chars>");

    await expect(handleRegistryRoute(
      jsonRequest("/v1/registry/aliases", {
        kind: "variant",
        alias: "codex",
        content_digest: "",
      }),
      new URL("https://cloud.example/v1/registry/aliases"),
      repository as unknown as RegistryRepository,
    )).rejects.toThrow("/content_digest must be a non-empty string");
  });
});

function jsonRequest(path: string, body: unknown): Request {
  return new Request(`https://cloud.example${path}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}

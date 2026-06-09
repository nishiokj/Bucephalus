import { describe, expect, test } from "bun:test";
import { handleRegistryRoute } from "../src/routes/registry";
import type { RegistryRepository } from "../src/registry/repository";
import { HttpError } from "../src/http";

describe("registry routes", () => {
  test("registry search without q lists inventory", async () => {
    const observed: { q?: string; limit?: number } = {};
    const repository = {
      async search(options: { q: string; limit: number }) {
        observed.q = options.q;
        observed.limit = options.limit;
        return [
          {
            kind: "agent_app",
            content_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            display_name: "Codex",
            aliases: ["codex"],
            score: 0,
            metadata: {},
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
    expect(body.hits).toHaveLength(1);
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

  test("registry search redacts public hit text and metadata", async () => {
    const repository = {
      async search() {
        return [
          {
            kind: "agent_app",
            content_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            display_name: "/Users/alice/private/agent token=raw-registry-display-token",
            aliases: ["codex-token=raw-registry-alias-token"],
            score: 1,
            metadata: {
              source_path: "/private/tmp/registry/agent.json",
              api_key: "sk-abcdefghijklmnopqrstuvwxyz",
            },
          },
        ];
      },
    };

    const response = await handleRegistryRoute(
      new Request("https://cloud.example/v1/registry/search?q=agent"),
      new URL("https://cloud.example/v1/registry/search?q=agent"),
      repository as unknown as RegistryRepository,
    );

    expect(response).not.toBeNull();
    const body = await response!.json();
    const text = JSON.stringify(body);

    expect(body.hits[0].display_name).toContain("[redacted-local-path]");
    expect(body.hits[0].display_name).toContain("token=[redacted-secret]");
    expect(body.hits[0].aliases[0]).toBe("codex-token=[redacted-secret]");
    expect(body.hits[0].metadata.source_path).toBe("[redacted-local-path]");
    expect(body.hits[0].metadata.api_key).toBe("[redacted]");
    expect(text).not.toContain("/Users/alice");
    expect(text).not.toContain("/private/tmp");
    expect(text).not.toContain("raw-registry-display-token");
    expect(text).not.toContain("raw-registry-alias-token");
    expect(text).not.toContain("sk-abcdefghijklmnopqrstuvwxyz");
  });

  test("registry object reads redact stored canonical JSON and aliases", async () => {
    const digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const repository = {
      async getContentObject() {
        return {
          content_digest: digest,
          kind: "agent_app",
          schema_version: "agent_app_v1",
          canonical_json: {
            display_name: "/Users/alice/private/Agent",
            launch: {
              access_token: "raw-registry-object-token",
              log_path: "file:///private/tmp/agent.log",
            },
          },
          canonical_size_bytes: 1,
          created_at: "2026-06-04T00:00:00Z",
          created_by: "user-token=raw-registry-created-by-token",
          source_uri: "file:///Users/alice/private/agent.yaml",
        };
      },
      async aliasesForDigest() {
        return [{
          alias_id: "alias-1",
          kind: "agent_app",
          alias: "agent-token=raw-registry-object-alias-token",
          scope_type: "workspace",
          scope_id: "/Users/alice/private/workspace",
          content_digest: digest,
        }];
      },
    };

    const response = await handleRegistryRoute(
      new Request(`https://cloud.example/v1/registry/objects/${digest}`),
      new URL(`https://cloud.example/v1/registry/objects/${digest}`),
      repository as unknown as RegistryRepository,
    );

    expect(response).not.toBeNull();
    const body = await response!.json();
    const text = JSON.stringify(body);

    expect(body.object.canonical_json.display_name).toBe("[redacted-local-path]");
    expect(body.object.canonical_json.launch.access_token).toBe("[redacted]");
    expect(body.object.canonical_json.launch.log_path).toBe("file://[redacted-local-path]");
    expect(body.object.created_by).toBe("user-token=[redacted-secret]");
    expect(body.object.source_uri).toBe("file://[redacted-local-path]");
    expect(body.aliases[0].alias).toBe("agent-token=[redacted-secret]");
    expect(body.aliases[0].scope_id).toBe("[redacted-local-path]");
    expect(text).not.toContain("/Users/alice");
    expect(text).not.toContain("/private/tmp");
    expect(text).not.toContain("raw-registry-object-token");
    expect(text).not.toContain("raw-registry-created-by-token");
    expect(text).not.toContain("raw-registry-object-alias-token");
  });

  test("registry review redacts existing objects, similar hits, and alias suggestions", async () => {
    const digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const repository = {
      async getContentObject() {
        return {
          content_digest: digest,
          kind: "agent_app",
          schema_version: "agent_app_v1",
          canonical_json: {
            id: "agent",
            display_name: "/Users/alice/private/Agent",
            bearer_token: "raw-registry-review-object-token",
          },
          canonical_size_bytes: 1,
          created_at: "2026-06-04T00:00:00Z",
          source_uri: "file:///private/tmp/agent.yaml",
        };
      },
      async aliasesForDigest() {
        return [{
          alias: "current-token=raw-registry-current-alias-token",
          scope_type: "workspace",
          scope_id: "/Users/alice/private/workspace",
          content_digest: digest,
        }];
      },
      async reviewAliases() {
        return [{
          alias: "candidate-token=raw-registry-review-alias-token",
          scope_type: "workspace",
          scope_id: "/Users/alice/private/workspace",
          status: "available",
          existing_digest: null,
        }];
      },
      async search() {
        return [{
          kind: "agent_app",
          content_digest: "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
          display_name: "similar /private/tmp/agent token=raw-registry-similar-token",
          aliases: ["similar-token=raw-registry-similar-alias-token"],
          score: 0.9,
          metadata: {},
        }];
      },
    };

    const response = await handleRegistryRoute(
      new Request("https://cloud.example/v1/registry/review", {
        method: "POST",
        body: JSON.stringify({
          kind: "agent_app",
          content_digest: digest,
          aliases: [{
            alias: "candidate-token=raw-registry-review-alias-token",
            scope_type: "workspace",
            scope_id: "/Users/alice/private/workspace",
          }],
        }),
      }),
      new URL("https://cloud.example/v1/registry/review"),
      repository as unknown as RegistryRepository,
    );

    expect(response).not.toBeNull();
    const body = await response!.json();
    const text = JSON.stringify(body);

    expect(body.canonical.canonical_json.display_name).toBe("[redacted-local-path]");
    expect(body.canonical.canonical_json.bearer_token).toBe("[redacted]");
    expect(body.exact_match.object.canonical_json.bearer_token).toBe("[redacted]");
    expect(body.exact_match.aliases[0].alias).toBe("current-token=[redacted-secret]");
    expect(body.alias_reviews[0].alias).toBe("candidate-token=[redacted-secret]");
    expect(body.alias_reviews[0].scope_id).toBe("[redacted-local-path]");
    expect(body.similar[0].display_name).toContain("[redacted-local-path]");
    expect(body.similar[0].display_name).toContain("token=[redacted-secret]");
    expect(body.suggested_actions.some((action: { alias?: string }) =>
      action.alias === "candidate-token=[redacted-secret]"
    )).toBe(true);
    expect(text).not.toContain("/Users/alice");
    expect(text).not.toContain("/private/tmp");
    expect(text).not.toContain("raw-registry-review-object-token");
    expect(text).not.toContain("raw-registry-current-alias-token");
    expect(text).not.toContain("raw-registry-review-alias-token");
    expect(text).not.toContain("raw-registry-similar-token");
    expect(text).not.toContain("raw-registry-similar-alias-token");
  });

  test("registry search rejects invalid limits before searching", async () => {
    let searchCalls = 0;
    const repository = {
      async search() {
        searchCalls += 1;
        return [];
      },
    };

    await expect(handleRegistryRoute(
      new Request("https://cloud.example/v1/registry/search?limit=token=raw-registry-limit"),
      new URL("https://cloud.example/v1/registry/search?limit=token=raw-registry-limit"),
      repository as unknown as RegistryRepository,
    )).rejects.toThrow("limit must be an integer from 1 to 200");
    expect(searchCalls).toBe(0);
  });

  test("registry digest inputs fail clearly before repository calls", async () => {
    let repositoryCalls = 0;
    const repository = {
      async getContentObject() {
        repositoryCalls += 1;
        return null;
      },
      async aliasesForDigest() {
        repositoryCalls += 1;
        return [];
      },
      async reviewAliases() {
        repositoryCalls += 1;
        return [];
      },
      async search() {
        repositoryCalls += 1;
        return [];
      },
      async hasDigest() {
        repositoryCalls += 1;
        return false;
      },
      async resolveAlias() {
        repositoryCalls += 1;
        return null;
      },
      async register() {
        repositoryCalls += 1;
        return true;
      },
      async createAlias() {
        repositoryCalls += 1;
        return {};
      },
    };

    for (const [request, url, message, rawSecret] of [
      [
        new Request("https://cloud.example/v1/registry/objects/sha256:not-hex-token=raw-object-secret"),
        new URL("https://cloud.example/v1/registry/objects/sha256:not-hex-token=raw-object-secret"),
        "/content_digest must be sha256:<64 lowercase hex chars>",
        "raw-object-secret",
      ],
      [
        new Request("https://cloud.example/v1/registry/review", {
          method: "POST",
          body: JSON.stringify({
            kind: "agent_app",
            content_digest: "sha256:not-hex-token=raw-review-secret",
          }),
        }),
        new URL("https://cloud.example/v1/registry/review"),
        "/content_digest must be sha256:<64 lowercase hex chars>",
        "raw-review-secret",
      ],
      [
        new Request("https://cloud.example/v1/registry/objects", {
          method: "POST",
          body: JSON.stringify({
            kind: "agent_app",
            schema_version: "v1",
            canonical_json: { id: "demo-agent" },
            expected_digest: "sha256:not-hex-token=raw-expected-secret",
          }),
        }),
        new URL("https://cloud.example/v1/registry/objects"),
        "/expected_digest must be sha256:<64 lowercase hex chars>",
        "raw-expected-secret",
      ],
      [
        new Request("https://cloud.example/v1/registry/resolve", {
          method: "POST",
          body: JSON.stringify({
            ref: {
              kind: "agent_app",
              digest: "sha256:not-hex-token=raw-resolve-secret",
            },
          }),
        }),
        new URL("https://cloud.example/v1/registry/resolve"),
        "/ref/digest must be sha256:<64 lowercase hex chars>",
        "raw-resolve-secret",
      ],
      [
        new Request("https://cloud.example/v1/registry/aliases", {
          method: "POST",
          body: JSON.stringify({
            kind: "agent_app",
            alias: "codex",
            content_digest: "sha256:not-hex-token=raw-alias-secret",
          }),
        }),
        new URL("https://cloud.example/v1/registry/aliases"),
        "/content_digest must be sha256:<64 lowercase hex chars>",
        "raw-alias-secret",
      ],
    ] as const) {
      let caught: unknown;
      try {
        await handleRegistryRoute(request, url, repository as unknown as RegistryRepository);
      } catch (error) {
        caught = error;
      }

      expect(caught).toBeInstanceOf(HttpError);
      const error = caught as HttpError;
      expect(error.status).toBe(400);
      expect(error.code).toBe("invalid_digest");
      expect(error.message).toBe(message);
      expect(JSON.stringify({ message: error.message, detail: error.detail })).not.toContain(rawSecret);
    }
    expect(repositoryCalls).toBe(0);
  });

  test("registry object paths fail clearly on malformed encoding before repository calls", async () => {
    let repositoryCalls = 0;
    const repository = {
      async getContentObject() {
        repositoryCalls += 1;
        return null;
      },
      async aliasesForDigest() {
        repositoryCalls += 1;
        return [];
      },
    };

    let caught: unknown;
    try {
      await handleRegistryRoute(
        new Request("https://cloud.example/v1/registry/objects/%E0%A4%A-token=raw-registry-path-secret"),
        new URL("https://cloud.example/v1/registry/objects/%E0%A4%A-token=raw-registry-path-secret"),
        repository as unknown as RegistryRepository,
      );
    } catch (error) {
      caught = error;
    }

    expect(caught).toBeInstanceOf(HttpError);
    const error = caught as HttpError;
    expect(error.status).toBe(400);
    expect(error.code).toBe("invalid_path_param");
    expect(error.message).toBe("/content_digest must be valid percent-encoded UTF-8");
    expect(JSON.stringify({ message: error.message, detail: error.detail })).not.toContain("raw-registry-path-secret");
    expect(repositoryCalls).toBe(0);
  });

  test("registry entity kinds fail clearly before repository calls", async () => {
    let repositoryCalls = 0;
    const repository = {
      async search() {
        repositoryCalls += 1;
        return [];
      },
      async getContentObject() {
        repositoryCalls += 1;
        return null;
      },
      async aliasesForDigest() {
        repositoryCalls += 1;
        return [];
      },
      async reviewAliases() {
        repositoryCalls += 1;
        return [];
      },
      async hasDigest() {
        repositoryCalls += 1;
        return false;
      },
      async resolveAlias() {
        repositoryCalls += 1;
        return null;
      },
      async register() {
        repositoryCalls += 1;
        return true;
      },
      async createAlias() {
        repositoryCalls += 1;
        return {};
      },
    };

    for (const [request, url, message, rawSecret] of [
      [
        new Request("https://cloud.example/v1/registry/search?kind=token=raw-kind-search-secret"),
        new URL("https://cloud.example/v1/registry/search?kind=token=raw-kind-search-secret"),
        "kind must be one of:",
        "raw-kind-search-secret",
      ],
      [
        new Request("https://cloud.example/v1/registry/canonicalize", {
          method: "POST",
          body: JSON.stringify({
            kind: "token=raw-kind-canonicalize-secret",
            object: {},
          }),
        }),
        new URL("https://cloud.example/v1/registry/canonicalize"),
        "/kind must be one of:",
        "raw-kind-canonicalize-secret",
      ],
      [
        new Request("https://cloud.example/v1/registry/review", {
          method: "POST",
          body: JSON.stringify({
            kind: "token=raw-kind-review-secret",
            content_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          }),
        }),
        new URL("https://cloud.example/v1/registry/review"),
        "/kind must be one of:",
        "raw-kind-review-secret",
      ],
      [
        new Request("https://cloud.example/v1/registry/resolve", {
          method: "POST",
          body: JSON.stringify({
            ref: {
              kind: "token=raw-kind-resolve-secret",
              alias: "codex",
            },
          }),
        }),
        new URL("https://cloud.example/v1/registry/resolve"),
        "/ref/kind must be one of:",
        "raw-kind-resolve-secret",
      ],
      [
        new Request("https://cloud.example/v1/registry/aliases", {
          method: "POST",
          body: JSON.stringify({
            kind: "token=raw-kind-alias-secret",
            alias: "codex",
            content_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          }),
        }),
        new URL("https://cloud.example/v1/registry/aliases"),
        "/kind must be one of:",
        "raw-kind-alias-secret",
      ],
    ] as const) {
      let caught: unknown;
      try {
        await handleRegistryRoute(request, url, repository as unknown as RegistryRepository);
      } catch (error) {
        caught = error;
      }

      expect(caught).toBeInstanceOf(HttpError);
      const error = caught as HttpError;
      expect(error.status).toBe(400);
      expect(error.code).toBe("invalid_entity_kind");
      expect(error.message).toContain(message);
      const encoded = JSON.stringify({ message: error.message, detail: error.detail });
      expect(encoded).toContain("agent_app");
      expect(encoded).toContain("trial_contract");
      expect(encoded).not.toContain(rawSecret);
    }
    expect(repositoryCalls).toBe(0);
  });
});

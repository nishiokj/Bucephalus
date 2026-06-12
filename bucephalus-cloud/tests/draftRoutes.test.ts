import { describe, expect, test } from "bun:test";
import { handleDraftRoute } from "../src/routes/drafts";
import type { RegistryRepository, RegistrySearchOptions } from "../src/registry/repository";
import type { EntityKind } from "../src/primitives";

describe("draft routes", () => {
  test("canonicalize returns a stable draft digest and inline entity bindings", async () => {
    const response = await handleDraftRoute(
      new Request("https://cloud.example/v1/drafts/canonicalize", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ draft: minimalDraft() }),
      }),
      new URL("https://cloud.example/v1/drafts/canonicalize"),
      {} as unknown as RegistryRepository,
    );

    expect(response).not.toBeNull();
    expect(response!.status).toBe(200);
    const body = await response!.json();
    expect(body.draft_digest).toMatch(/^sha256:[a-f0-9]{64}$/);
    expect(body.canonical_draft.experiment.id).toBe("demo");
    expect(body.digest_map).toContainEqual(expect.objectContaining({
      pointer: "/matrix/variants/0",
      kind: "variant",
      content_digest: expect.stringMatching(/^sha256:[a-f0-9]{64}$/),
      resolution: "inline",
      display_name: "baseline",
    }));
  });

  test("resolve annotates registry alias bindings and unresolved refs", async () => {
    const repository = {
      async hasDigest() {
        return false;
      },
      async resolveAlias(kind: EntityKind, alias: string) {
        if (kind === "variant" && alias === "baseline") {
          return "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        }
        return null;
      },
    };
    const draft = {
      ...minimalDraft(),
      matrix: {
        ...minimalDraft().matrix,
        variants: [{
          id: "baseline",
          registry: {
            alias: "baseline",
          },
        }],
      },
    };

    const response = await handleDraftRoute(
      new Request("https://cloud.example/v1/drafts/resolve", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ draft }),
      }),
      new URL("https://cloud.example/v1/drafts/resolve"),
      repository as unknown as RegistryRepository,
    );

    expect(response).not.toBeNull();
    expect(response!.status).toBe(200);
    const body = await response!.json();
    expect(body.bindings).toContainEqual(expect.objectContaining({
      pointer: "/matrix/variants/0",
      kind: "variant",
      content_digest: "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      resolution: "alias",
      alias: "baseline",
      display_name: "baseline",
    }));
    expect(body.resolved_draft.matrix.variants[0].__cloud.digest).toBe(
      "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    );
    expect(body.unresolved).toEqual([]);
  });

  test("suggest returns registry-backed suggestions plus draft warnings", async () => {
    const observed: { kind?: EntityKind | undefined; q?: string; limit?: number } = {};
    const repository = {
      async search(options: RegistrySearchOptions) {
        observed.kind = options.kind;
        observed.q = options.q;
        observed.limit = options.limit;
        return [{
          kind: "variant",
          content_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          display_name: "Baseline Variant",
          aliases: ["baseline"],
          score: 0.9,
          metadata: {},
        }];
      },
      async hasDigest() {
        return false;
      },
      async resolveAlias() {
        return null;
      },
    };

    const response = await handleDraftRoute(
      new Request("https://cloud.example/v1/drafts/suggest", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          draft: minimalDraft(),
          target: "variant",
          q: "base",
          limit: 3,
        }),
      }),
      new URL("https://cloud.example/v1/drafts/suggest"),
      repository as unknown as RegistryRepository,
    );

    expect(response).not.toBeNull();
    expect(response!.status).toBe(200);
    expect(observed).toEqual({ kind: "variant", q: "base", limit: 3 });
    const body = await response!.json();
    expect(body.suggestions).toContainEqual(expect.objectContaining({
      suggestion_type: "registry_entity",
      title: "Baseline Variant",
      registry_hit: expect.objectContaining({
        content_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      }),
    }));
    expect(body.suggestions).toContainEqual(expect.objectContaining({
      suggestion_type: "warning",
      title: "default_repeats",
    }));
  });

  test("suggest rejects limits outside the hosted authoring contract", async () => {
    const repository = {
      async search() {
        throw new Error("registry search should not be called");
      },
    };

    await expect(handleDraftRoute(
      new Request("https://cloud.example/v1/drafts/suggest", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          draft: minimalDraft(),
          target: "variant",
          limit: 101,
        }),
      }),
      new URL("https://cloud.example/v1/drafts/suggest"),
      repository as unknown as RegistryRepository,
    )).rejects.toThrow("/limit must be <= 100");
  });

  test("validate applies requested package-level checks", async () => {
    const draft = {
      ...minimalDraft(),
      matrix: {
        ...minimalDraft().matrix,
        cases: {},
      },
    };

    const response = await handleDraftRoute(
      new Request("https://cloud.example/v1/drafts/validate", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          draft,
          validation_level: "package",
        }),
      }),
      new URL("https://cloud.example/v1/drafts/validate"),
      {} as unknown as RegistryRepository,
    );

    expect(response).not.toBeNull();
    expect(response!.status).toBe(200);
    const body = await response!.json();
    expect(body.validation_level).toBe("package");
    expect(body.valid).toBe(false);
    expect(body.issues).toContainEqual(expect.objectContaining({
      severity: "error",
      code: "missing_cases_source",
      pointer: "/matrix/cases",
    }));
  });

  test("validate launch hints surface cloud runtime dependencies without failing warnings", async () => {
    const draft = {
      ...minimalDraft(),
      runtime: {
        compute: {
          backend: "modal",
        },
        secrets: [{
          name: "GEMINI_API_KEY",
          mount: {
            target: "/run/secrets/gemini_api_key",
          },
        }],
        network: {
          egress: ["generativelanguage.googleapis.com"],
        },
      },
    };

    const response = await handleDraftRoute(
      new Request("https://cloud.example/v1/drafts/validate", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          draft,
          validation_level: "launch_hint",
        }),
      }),
      new URL("https://cloud.example/v1/drafts/validate"),
      {} as unknown as RegistryRepository,
    );

    expect(response).not.toBeNull();
    expect(response!.status).toBe(200);
    const body = await response!.json();
    expect(body.validation_level).toBe("launch_hint");
    expect(body.valid).toBe(true);
    expect(body.issues).toContainEqual(expect.objectContaining({
      severity: "info",
      code: "cloud_secret_ref_required",
      pointer: "/runtime/secrets/0",
    }));
    expect(body.issues).toContainEqual(expect.objectContaining({
      severity: "info",
      code: "cloud_network_requirements",
      pointer: "/runtime/network",
    }));
  });

  test("validate rejects unknown validation levels instead of silently downgrading", async () => {
    await expect(handleDraftRoute(
      new Request("https://cloud.example/v1/drafts/validate", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          draft: minimalDraft(),
          validation_level: "runtime",
        }),
      }),
      new URL("https://cloud.example/v1/drafts/validate"),
      {} as unknown as RegistryRepository,
    )).rejects.toThrow("/validation_level must be one of authoring, package, launch_hint");
  });

  test("diff compares draft sides with JSON pointer changes", async () => {
    const repository = {
      async getContentObject() {
        throw new Error("registry should not be used for inline draft diff");
      },
    };
    const left = minimalDraft();
    const right = {
      ...minimalDraft(),
      experiment: {
        id: "demo",
        name: "Demo Renamed",
      },
      runtime: {
        compute: {
          backend: "modal",
        },
      },
    };

    const response = await handleDraftRoute(
      new Request("https://cloud.example/v1/drafts/diff", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          left: { draft: left },
          right: { draft: right },
        }),
      }),
      new URL("https://cloud.example/v1/drafts/diff"),
      repository as unknown as RegistryRepository,
    );

    expect(response).not.toBeNull();
    expect(response!.status).toBe(200);
    const body = await response!.json();
    expect(body.left).toEqual({ kind: "experiment_package", inline: left });
    expect(body.right).toEqual({ kind: "experiment_package", inline: right });
    expect(body.changes).toContainEqual(expect.objectContaining({
      op: "replace",
      pointer: "/experiment/name",
      left: "Demo",
      right: "Demo Renamed",
      significance: "presentation",
    }));
    expect(body.changes).toContainEqual(expect.objectContaining({
      op: "replace",
      pointer: "/runtime/compute/backend",
      left: "local-docker",
      right: "modal",
      significance: "behavior",
    }));
  });
});

function minimalDraft() {
  return {
    experiment: {
      id: "demo",
      name: "Demo",
    },
    runtime: {
      compute: {
        backend: "local-docker",
      },
    },
    matrix: {
      variants: [{
        id: "baseline",
      }],
      cases: {
        count: 2,
      },
    },
    stages: {},
    policy: {},
  };
}

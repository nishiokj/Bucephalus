import { describe, expect, test } from "bun:test";
import { HttpError } from "../src/http";
import { handleDraftRoute } from "../src/routes/drafts";
import type { RegistryRepository } from "../src/registry/repository";

describe("draft routes", () => {
  test("draft export rejects unsupported formats before registry calls", async () => {
    let repositoryCalls = 0;
    const repository = {
      async hasDigest() {
        repositoryCalls += 1;
        return false;
      },
      async resolveAlias() {
        repositoryCalls += 1;
        return null;
      },
    };

    let caught: unknown;
    try {
      await handleDraftRoute(
        jsonRequest("https://cloud.example/v1/drafts/export", {
          draft: minimalDraft(),
          format: "token=raw-draft-format-secret",
        }),
        new URL("https://cloud.example/v1/drafts/export"),
        repository as unknown as RegistryRepository,
      );
    } catch (error) {
      caught = error;
    }

    expect(caught).toBeInstanceOf(HttpError);
    const error = caught as HttpError;
    expect(error.status).toBe(400);
    expect(error.code).toBe("unsupported_draft_export_format");
    expect(error.message).toBe("/format must be one of: yaml, resolved_json");
    expect(JSON.stringify({ message: error.message, detail: error.detail })).not.toContain("raw-draft-format-secret");
    expect(repositoryCalls).toBe(0);
  });

  test("draft YAML export failures return a curated fallback error", async () => {
    const repository = {
      async hasDigest() {
        return false;
      },
      async resolveAlias() {
        return null;
      },
    };

    let caught: unknown;
    try {
      await handleDraftRoute(
        jsonRequest("https://cloud.example/v1/drafts/export", {
          draft: minimalDraft(),
          format: "yaml",
        }),
        new URL("https://cloud.example/v1/drafts/export"),
        repository as unknown as RegistryRepository,
        {
          exportDraftYaml() {
            throw new Error(
              "python3 failed in /Users/alice/private/.venv with token=raw-draft-yaml-export-secret",
            );
          },
        },
      );
    } catch (error) {
      caught = error;
    }

    expect(caught).toBeInstanceOf(HttpError);
    const error = caught as HttpError;
    const text = JSON.stringify({ message: error.message, detail: error.detail });

    expect(error.status).toBe(503);
    expect(error.code).toBe("draft_export_unavailable");
    expect(error.message).toBe("Draft YAML export is unavailable; retry with format=resolved_json or try again later");
    expect(error.detail).toEqual({
      format: "yaml",
      fallback_format: "resolved_json",
    });
    expect(text).not.toContain("/Users/alice");
    expect(text).not.toContain("raw-draft-yaml-export-secret");
    expect(text).not.toContain("python3 failed");
  });

  test("draft resolved JSON export serializes the resolved draft", async () => {
    const digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const repository = {
      async hasDigest() {
        return false;
      },
      async resolveAlias() {
        return digest;
      },
    };
    const draft = minimalDraft({
      matrix: {
        variants: [{
          registry: {
            alias: "baseline",
          },
        }],
        cases: { count: 1 },
        repeats: 1,
      },
    });

    const response = await handleDraftRoute(
      jsonRequest("https://cloud.example/v1/drafts/export", {
        draft,
        format: "resolved_json",
      }),
      new URL("https://cloud.example/v1/drafts/export"),
      repository as unknown as RegistryRepository,
    );
    const body = await response!.json();
    const exported = JSON.parse(body.body);

    expect(body.format).toBe("resolved_json");
    expect(exported.matrix.variants[0].__cloud.digest).toBe(digest);
  });

  test("draft unresolved reference feedback is public-boundary redacted", async () => {
    const repository = {
      async hasDigest() {
        return false;
      },
      async resolveAlias() {
        return null;
      },
    };
    const draft = minimalDraft({
      matrix: {
        variants: [{
          registry: {
            alias: "token=raw-draft-alias-secret /Users/alice/private/variant",
          },
        }],
        cases: { count: 1 },
        repeats: 1,
      },
    });

    const response = await handleDraftRoute(
      jsonRequest("https://cloud.example/v1/drafts/resolve", { draft }),
      new URL("https://cloud.example/v1/drafts/resolve"),
      repository as unknown as RegistryRepository,
    );
    const body = await response!.json();
    const unresolved = body.unresolved[0];

    expect(unresolved.reason).toContain("token=[redacted-secret]");
    expect(unresolved.reason).toContain("[redacted-local-path]");
    expect(unresolved.reason).not.toContain("raw-draft-alias-secret");
    expect(unresolved.reason).not.toContain("/Users/alice");
  });

  test("draft malformed registry digests fail before repository calls", async () => {
    let repositoryCalls = 0;
    const repository = {
      async hasDigest() {
        repositoryCalls += 1;
        return false;
      },
      async resolveAlias() {
        repositoryCalls += 1;
        return null;
      },
    };
    const draft = minimalDraft({
      matrix: {
        variants: [{
          registry: {
            digest: "sha256:not-hex-token=raw-draft-digest-secret",
          },
        }],
        cases: { count: 1 },
        repeats: 1,
      },
    });

    const response = await handleDraftRoute(
      jsonRequest("https://cloud.example/v1/drafts/resolve", { draft }),
      new URL("https://cloud.example/v1/drafts/resolve"),
      repository as unknown as RegistryRepository,
    );
    const body = await response!.json();
    const unresolved = body.unresolved[0];

    expect(unresolved.reason).toBe("registry.digest must be sha256:<64 lowercase hex chars>");
    expect(JSON.stringify(unresolved)).not.toContain("raw-draft-digest-secret");
    expect(repositoryCalls).toBe(0);
  });

  test("draft resolved ref summaries redact aliases and display names", async () => {
    const repository = {
      async hasDigest() {
        return false;
      },
      async resolveAlias() {
        return "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
      },
    };
    const draft = minimalDraft({
      matrix: {
        variants: [{
          display_name: "/Users/alice/private/variant token=raw-draft-display-secret",
          registry: {
            alias: "token=raw-draft-alias-secret",
          },
        }],
        cases: { count: 1 },
        repeats: 1,
      },
    });

    const response = await handleDraftRoute(
      jsonRequest("https://cloud.example/v1/drafts/validate", { draft }),
      new URL("https://cloud.example/v1/drafts/validate"),
      repository as unknown as RegistryRepository,
    );
    const body = await response!.json();
    const text = JSON.stringify(body);

    expect(body.resolved_refs[0].alias).toBe("token=[redacted-secret]");
    expect(body.resolved_refs[0].display_name).toContain("[redacted-local-path]");
    expect(body.resolved_refs[0].display_name).toContain("token=[redacted-secret]");
    expect(text).not.toContain("raw-draft-alias-secret");
    expect(text).not.toContain("raw-draft-display-secret");
    expect(text).not.toContain("/Users/alice");
  });

  test("draft preview and validation flag invalid schedule counts", async () => {
    const repository = {
      async hasDigest() {
        return false;
      },
      async resolveAlias() {
        return null;
      },
    };
    const draft = minimalDraft({
      matrix: {
        variants: [{ id: "base" }, { id: "next" }],
        cases: { count: -3 },
        repeats: 1.5,
      },
      scheduling: {
        max_concurrency: 0,
      },
    });

    const previewResponse = await handleDraftRoute(
      jsonRequest("https://cloud.example/v1/drafts/preview-schedule", { draft }),
      new URL("https://cloud.example/v1/drafts/preview-schedule"),
      repository as unknown as RegistryRepository,
    );
    const preview = await previewResponse!.json();
    const previewCodes = preview.warnings.map((issue: { code: string }) => issue.code);

    expect(preview.total_slots).toBeNull();
    expect(preview.cases).toBeNull();
    expect(preview.repeats).toBe(1);
    expect(preview.max_concurrency).toBeNull();
    expect(previewCodes).toContain("invalid_case_count");
    expect(previewCodes).toContain("invalid_matrix_repeats");
    expect(previewCodes).toContain("invalid_max_concurrency");

    const validationResponse = await handleDraftRoute(
      jsonRequest("https://cloud.example/v1/drafts/validate", { draft }),
      new URL("https://cloud.example/v1/drafts/validate"),
      repository as unknown as RegistryRepository,
    );
    const validation = await validationResponse!.json();
    const validationCodes = validation.issues.map((issue: { code: string }) => issue.code);

    expect(validation.valid).toBe(false);
    expect(validationCodes).toContain("invalid_case_count");
    expect(validationCodes).toContain("invalid_matrix_repeats");
    expect(validationCodes).toContain("invalid_max_concurrency");
  });

  test("draft preview warnings use the public issue boundary", async () => {
    const repository = {
      async hasDigest() {
        return false;
      },
      async resolveAlias() {
        return null;
      },
    };
    const draft = minimalDraft({
      matrix: {
        variants: [{ id: "base" }],
        cases: {
          source: "file",
          path: "/Users/alice/private/cases-token=raw-draft-preview-token.jsonl",
        },
      },
    });

    const response = await handleDraftRoute(
      jsonRequest("https://cloud.example/v1/drafts/preview-schedule", { draft }),
      new URL("https://cloud.example/v1/drafts/preview-schedule"),
      repository as unknown as RegistryRepository,
    );
    const body = await response!.json();
    const text = JSON.stringify(body);

    expect(body.warnings[0].code).toBe("default_repeats");
    expect(body.warnings.some((issue: { code: string }) => issue.code === "case_count_unknown")).toBe(true);
    expect(text).not.toContain("/Users/alice");
    expect(text).not.toContain("raw-draft-preview-token");
  });
});

function jsonRequest(url: string, body: unknown): Request {
  return new Request(url, {
    method: "POST",
    headers: {
      "content-type": "application/json",
    },
    body: JSON.stringify(body),
  });
}

function minimalDraft(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    experiment: {
      id: "demo",
      name: "Demo",
    },
    runtime: {
      compute: {
        backend: "runner-docker",
      },
    },
    matrix: {
      variants: [{
        id: "base",
      }],
      cases: {
        count: 1,
      },
      repeats: 1,
    },
    stages: {},
    policy: {},
    ...overrides,
  };
}

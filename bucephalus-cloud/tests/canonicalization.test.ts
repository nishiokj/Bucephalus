import { describe, expect, test } from "bun:test";
import {
  CanonicalizationError,
  canonicalJsonStringify,
  canonicalizeEntity,
  normalizationHints,
  resolveRegistryRef,
  type CanonicalEntity,
  type EntityKind,
} from "../src/primitives";

describe("canonicalization", () => {
  test("hashes the canonical envelope with stable object key ordering", () => {
    const left = canonicalizeEntity({
      kind: "variant",
      schemaVersion: "variant_v1",
      object: {
        config: {
          temperature: 0,
          model: "gpt-5",
        },
        id: "codex",
      },
    });
    const right = canonicalizeEntity({
      kind: "variant",
      schemaVersion: "variant_v1",
      object: {
        id: "codex",
        config: {
          model: "gpt-5",
          temperature: 0,
        },
      },
    });

    expect(left.contentDigest).toBe(right.contentDigest);
    expect(new TextDecoder().decode(left.canonicalBytes)).toBe(
      '{"canonical_json":{"config":{"model":"gpt-5","temperature":0},"id":"codex"},"kind":"variant","protocol":"bucephalus-canonical-json-v1","schema_version":"variant_v1"}',
    );
  });

  test("includes kind and schema version in identity", () => {
    const object = { id: "same-shape" };
    const variant = canonicalizeEntity({
      kind: "variant",
      schemaVersion: "v1",
      object,
    });
    const metric = canonicalizeEntity({
      kind: "metric",
      schemaVersion: "v1",
      object,
    });
    const variantV2 = canonicalizeEntity({
      kind: "variant",
      schemaVersion: "v2",
      object,
    });

    expect(variant.contentDigest).not.toBe(metric.contentDigest);
    expect(variant.contentDigest).not.toBe(variantV2.contentDigest);
  });

  test("rejects values that JSON cannot faithfully represent", () => {
    expect(() =>
      canonicalizeEntity({
        kind: "variant",
        schemaVersion: "v1",
        object: { value: Number.NaN },
      }),
    ).toThrow(CanonicalizationError);

    expect(() =>
      canonicalizeEntity({
        kind: "variant",
        schemaVersion: "v1",
        object: { value: undefined as never },
      }),
    ).toThrow(CanonicalizationError);

    expect(() =>
      canonicalizeEntity({
        kind: "variant",
        schemaVersion: "v1",
        object: { value: -0 },
      }),
    ).toThrow(CanonicalizationError);
  });

  test("canonical JSON stringification is deterministic for nested arrays and objects", () => {
    expect(canonicalJsonStringify({ b: [2, { d: true, c: null }], a: "x" })).toBe(
      '{"a":"x","b":[2,{"c":null,"d":true}]}',
    );
  });
});

describe("normalization hints", () => {
  test("returns suggestions without applying them", () => {
    const report = normalizationHints({
      kind: "variant",
      object: { id: "baseline", name: "Baseline", config: { model: "gpt-5" } },
      similarDigests: [
        {
          digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          score: 0.91,
          displayName: "codex baseline",
        },
      ],
    });

    expect(report.applied).toBe(false);
    expect(report.suggestions.map((suggestion) => suggestion.code)).toEqual([
      "display_id_is_not_identity",
      "name_is_presentation",
      "similar_registered_entity",
    ]);
  });
});

describe("registry ref resolution", () => {
  test("does not register inline refs unless explicitly requested", async () => {
    const registered = new Map<string, CanonicalEntity>();
    const repository = {
      hasDigest: (_kind: EntityKind, digest: string) => registered.has(digest),
      resolveAlias: () => null,
      register: (entity: CanonicalEntity) => {
        registered.set(entity.contentDigest, entity);
      },
    };

    const first = await resolveRegistryRef(
      {
        kind: "variant",
        schemaVersion: "v1",
        inline: { id: "baseline" },
      },
      repository,
    );
    expect(first.resolution).toBe("inline_unregistered");
    expect(registered.size).toBe(0);

    const second = await resolveRegistryRef(
      {
        kind: "variant",
        schemaVersion: "v1",
        inline: { id: "baseline" },
      },
      repository,
      { registerInlineIfMissing: true },
    );
    expect(second.resolution).toBe("inline_registered");
    expect(registered.size).toBe(1);
  });
});


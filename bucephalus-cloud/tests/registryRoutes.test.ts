import { describe, expect, test } from "bun:test";
import { handleRegistryRoute } from "../src/routes/registry";
import type { RegistryRepository } from "../src/registry/repository";

describe("registry routes", () => {
  test("registry search without q lists inventory", async () => {
    const observed: { q?: string; limit?: number } = {};
    const repository = {
      async search(options: { q: string; limit: number }) {
        observed.q = options.q;
        observed.limit = options.limit;
        return [
          {
            kind: "agent",
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
});

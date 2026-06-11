import { describe, expect, test } from "bun:test";
import { handleSecretRoute } from "../src/routes/secrets";
import type { AuthContext } from "../src/auth";
import type { CloudSecretRecord, CloudSecretRepository } from "../src/secrets/repository";
import type { SecretStore } from "../src/secrets/store";

describe("hosted secret routes", () => {
  test("uploads a secret without echoing the value back", async () => {
    const harness = secretHarness();
    const response = await handleSecretRoute(
      putRequest("OPENAI_API_KEY", "sk-super-secret"),
      new URL("https://cloud.example/v1/secrets/OPENAI_API_KEY"),
      harness.repository,
      harness.store,
      authContext("user-a"),
    );

    expect(response).not.toBeNull();
    expect(response!.status).toBe(201);
    const body = await response!.json();
    expect(body).toEqual({
      name: "OPENAI_API_KEY",
      version: 1,
      created_at: "2026-06-11T00:00:00Z",
      updated_at: "2026-06-11T00:00:00Z",
    });
    expect(JSON.stringify(body)).not.toContain("sk-super-secret");
    expect(harness.written).toEqual([{ ownerKey: "issuer:user-a", name: "OPENAI_API_KEY", value: "sk-super-secret" }]);
  });

  test("rotating an existing secret returns 200 with a bumped version", async () => {
    const harness = secretHarness({ existing: record("OPENAI_API_KEY", 1) });
    const response = await handleSecretRoute(
      putRequest("OPENAI_API_KEY", "sk-rotated"),
      new URL("https://cloud.example/v1/secrets/OPENAI_API_KEY"),
      harness.repository,
      harness.store,
      authContext("user-a"),
    );
    expect(response!.status).toBe(200);
    expect((await response!.json()).version).toBe(2);
  });

  test("rejects control-plane reserved names and invalid names", async () => {
    const harness = secretHarness();
    await expect(handleSecretRoute(
      putRequest("worker-token", "value"),
      new URL("https://cloud.example/v1/secrets/worker-token"),
      harness.repository,
      harness.store,
      authContext("user-a"),
    )).rejects.toThrow("reserved for Cloud control-plane credentials");

    await expect(handleSecretRoute(
      putRequest("not%20valid", "value"),
      new URL("https://cloud.example/v1/secrets/not%20valid"),
      harness.repository,
      harness.store,
      authContext("user-a"),
    )).rejects.toThrow("Invalid secret name");
    expect(harness.written).toHaveLength(0);
  });

  test("rejects oversized secret values", async () => {
    const harness = secretHarness();
    await expect(handleSecretRoute(
      putRequest("BIG", "x".repeat(64 * 1024 + 1)),
      new URL("https://cloud.example/v1/secrets/BIG"),
      harness.repository,
      harness.store,
      authContext("user-a"),
    )).rejects.toThrow("64 KiB");
  });

  test("lists names and metadata only", async () => {
    const harness = secretHarness({ existing: record("OPENAI_API_KEY", 3) });
    const response = await handleSecretRoute(
      new Request("https://cloud.example/v1/secrets"),
      new URL("https://cloud.example/v1/secrets"),
      harness.repository,
      harness.store,
      authContext("user-a"),
    );
    const body = await response!.json();
    expect(body.secrets).toEqual([
      {
        name: "OPENAI_API_KEY",
        version: 3,
        created_at: "2026-06-11T00:00:00Z",
        updated_at: "2026-06-11T00:00:00Z",
      },
    ]);
    expect(JSON.stringify(body)).not.toContain("backing_ref");
    expect(JSON.stringify(body)).not.toContain("store_name");
  });

  test("delete removes the record and the backing store entry", async () => {
    const harness = secretHarness({ existing: record("OPENAI_API_KEY", 1) });
    const response = await handleSecretRoute(
      new Request("https://cloud.example/v1/secrets/OPENAI_API_KEY", { method: "DELETE" }),
      new URL("https://cloud.example/v1/secrets/OPENAI_API_KEY"),
      harness.repository,
      harness.store,
      authContext("user-a"),
    );
    expect(await response!.json()).toEqual({ name: "OPENAI_API_KEY", deleted: true });
    expect(harness.removed).toEqual(["store-OPENAI_API_KEY"]);
  });

  test("requires an authenticated owner", async () => {
    const harness = secretHarness();
    await expect(handleSecretRoute(
      new Request("https://cloud.example/v1/secrets"),
      new URL("https://cloud.example/v1/secrets"),
      harness.repository,
      harness.store,
    )).rejects.toThrow("requires an authenticated user");
  });
});

function secretHarness(input: { existing?: CloudSecretRecord } = {}) {
  const written: { ownerKey: string; name: string; value: string }[] = [];
  const removed: string[] = [];
  let current = input.existing ?? null;

  const store = {
    async put(ownerKey: string, name: string, value: string) {
      written.push({ ownerKey, name, value });
      return { storeName: `store-${name}`, backingRef: `file:/data/secrets/store-${name}` };
    },
    async remove(storeName: string) {
      removed.push(storeName);
    },
  };

  const repository = {
    async upsertSecret(input: { ownerKey: string; name: string; storeName: string; backingRef: string }) {
      const created = current === null;
      current = {
        ...record(input.name, created ? 1 : current!.version + 1),
        owner_key: input.ownerKey,
        store_name: input.storeName,
        backing_ref: input.backingRef,
      };
      return { record: current, created };
    },
    async getSecret() {
      return current;
    },
    async listSecrets() {
      return current ? [current] : [];
    },
    async deleteSecret() {
      const deleted = current;
      current = null;
      return deleted;
    },
  };

  return {
    written,
    removed,
    store: store as unknown as SecretStore,
    repository: repository as unknown as CloudSecretRepository,
  };
}

function record(name: string, version: number): CloudSecretRecord {
  return {
    secret_id: "secret-1",
    owner_key: "issuer:user-a",
    name,
    store_name: `store-${name}`,
    backing_ref: `file:/data/secrets/store-${name}`,
    version,
    created_at: "2026-06-11T00:00:00Z",
    updated_at: "2026-06-11T00:00:00Z",
  };
}

function putRequest(name: string, value: string): Request {
  return new Request(`https://cloud.example/v1/secrets/${name}`, {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ value }),
  });
}

function authContext(subject: string): AuthContext {
  return {
    subject,
    issuer: "issuer",
    audience: "audience",
    claims: { sub: subject, iss: "issuer", aud: "audience" },
  };
}

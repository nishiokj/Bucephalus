import { mkdtemp, readFile, rm, stat } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { describe, expect, test } from "bun:test";
import {
  FilesystemSecretStoreBackend,
  GcpSecretStoreBackend,
  SecretStore,
  storeSecretName,
} from "../src/secrets/store";

describe("hosted secret store", () => {
  test("store names are deterministic per owner and never embed the owner key", () => {
    const first = storeSecretName("buc", "issuer:user-a", "OPENAI_API_KEY");
    const second = storeSecretName("buc", "issuer:user-a", "OPENAI_API_KEY");
    const otherOwner = storeSecretName("buc", "issuer:user-b", "OPENAI_API_KEY");
    expect(first).toBe(second);
    expect(first).not.toBe(otherOwner);
    expect(first).not.toContain("issuer");
    expect(first).not.toContain("user-a");
    expect(first).toMatch(/^buc-[0-9a-f]{16}-OPENAI_API_KEY$/);
  });

  test("filesystem backend writes owner-scoped files with restricted permissions", async () => {
    const dataDir = await mkdtemp(join(tmpdir(), "buc-secrets-"));
    try {
      const store = new SecretStore(new FilesystemSecretStoreBackend(dataDir), "buc");
      const { storeName, backingRef } = await store.put("issuer:user-a", "OPENAI_API_KEY", "sk-test-value");
      expect(backingRef.startsWith("file:/")).toBe(true);
      const path = backingRef.slice("file:".length);
      expect(await readFile(path, "utf8")).toBe("sk-test-value");
      expect(((await stat(path)).mode & 0o777)).toBe(0o600);

      await store.put("issuer:user-a", "OPENAI_API_KEY", "sk-rotated");
      expect(await readFile(path, "utf8")).toBe("sk-rotated");

      await store.remove(storeName);
      await expect(stat(path)).rejects.toThrow();
    } finally {
      await rm(dataDir, { recursive: true, force: true });
    }
  });

  test("gcp backend creates the secret, adds a version, and pins the backing ref", async () => {
    const requests: { url: string; method: string; body?: unknown }[] = [];
    const fetchImpl = async (input: string | URL | Request, init?: RequestInit) => {
      const url = String(input);
      requests.push({
        url,
        method: init?.method ?? "GET",
        body: typeof init?.body === "string" ? JSON.parse(init.body) : undefined,
      });
      if (url.includes("metadata.google.internal")) {
        return Response.json({ access_token: "metadata-token" });
      }
      if (url.includes(":addVersion")) {
        return Response.json({ name: "projects/123456/secrets/buc-abc/versions/7" });
      }
      return Response.json({}, { status: 409 });
    };

    const backend = new GcpSecretStoreBackend("bucephalus-prod", fetchImpl);
    const { backingRef } = await backend.write("buc-abc", "sk-test-value");
    expect(backingRef).toBe("gcp-secret-manager://projects/bucephalus-prod/secrets/buc-abc/versions/7");

    const addVersion = requests.find((request) => request.url.includes(":addVersion"));
    expect(addVersion?.method).toBe("POST");
    expect((addVersion?.body as { payload: { data: string } }).payload.data)
      .toBe(Buffer.from("sk-test-value", "utf8").toString("base64"));
    const create = requests.find((request) => request.url.includes("secretId=buc-abc"));
    expect(create?.method).toBe("POST");
  });

  test("gcp backend surfaces addVersion failures", async () => {
    const fetchImpl = async (input: string | URL | Request) => {
      if (String(input).includes("metadata.google.internal")) {
        return Response.json({ access_token: "metadata-token" });
      }
      return new Response("permission denied", { status: 403 });
    };
    const backend = new GcpSecretStoreBackend("bucephalus-prod", fetchImpl);
    await expect(backend.write("buc-abc", "value")).rejects.toThrow("HTTP 403");
  });
});

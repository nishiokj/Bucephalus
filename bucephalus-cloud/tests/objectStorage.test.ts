import { mkdir, mkdtemp, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, test } from "bun:test";
import { loadConfig } from "../src/config";
import { materializeStoredObject, putUploadObject, readStoredObject } from "../src/objectStorage";

describe("object storage", () => {
  test("stores uploads on the filesystem by default", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-object-storage-"));
    try {
      const config = loadConfig({
        BUCEPHALUS_CLOUD_DATA_DIR: root,
      });
      const storagePath = await putUploadObject("upload-1", bytes("package bytes"), "application/gzip", config);

      expect(storagePath).toBe(join(root, "uploads", "upload-1", "content.blob"));
      expect(await readFile(storagePath, "utf8")).toBe("package bytes");
      expect(await readStoredObject(storagePath, config)).toEqual(bytes("package bytes"));
      expect(await materializeStoredObject(storagePath, join(root, "work"), "package.blob", config)).toBe(storagePath);
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("rejects filesystem object paths outside configured upload storage", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-object-storage-"));
    try {
      const config = loadConfig({
        BUCEPHALUS_CLOUD_DATA_DIR: join(root, "data"),
      });
      const secretPath = join(root, "customer-a-prod-openai-token.env");
      await writeFile(secretPath, "secret");

      for (const action of [
        () => readStoredObject(secretPath, config),
        () => materializeStoredObject(secretPath, join(root, "work"), "package.blob", config),
      ]) {
        let caught: unknown;
        try {
          await action();
        } catch (error) {
          caught = error;
        }

        expect(caught).toBeInstanceOf(Error);
        const text = JSON.stringify({
          name: (caught as Error).name,
          message: (caught as Error).message,
        });
        expect(text).toContain("configured filesystem upload storage");
        expect(text).not.toContain("customer-a");
        expect(text).not.toContain("openai-token");
      }
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("rejects filesystem upload objects that resolve outside storage", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-object-storage-"));
    try {
      const dataDir = join(root, "data");
      const uploadDir = join(dataDir, "uploads", "upload-1");
      const storagePath = join(uploadDir, "content.blob");
      const secretPath = join(root, "customer-a-prod-openai-token.env");
      await mkdir(uploadDir, { recursive: true });
      await writeFile(secretPath, "secret");
      await symlink(secretPath, storagePath);

      let caught: unknown;
      try {
        await readStoredObject(storagePath, loadConfig({
          BUCEPHALUS_CLOUD_DATA_DIR: dataDir,
        }));
      } catch (error) {
        caught = error;
      }

      expect(caught).toBeInstanceOf(Error);
      const text = JSON.stringify({
        name: (caught as Error).name,
        message: (caught as Error).message,
      });
      expect(text).toContain("resolves outside");
      expect(text).not.toContain("customer-a");
      expect(text).not.toContain("openai-token");
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("writes upload objects to R2 using the S3-compatible endpoint", async () => {
    const requests: Array<{ url: string; init: RequestInit | undefined }> = [];
    const previousFetch = globalThis.fetch;
    globalThis.fetch = (async (url: string | URL | Request, init?: RequestInit) => {
      requests.push({ url: String(url), init });
      return new Response(null, { status: 200 });
    }) as typeof fetch;
    try {
      const config = r2Config();
      const storagePath = await putUploadObject("upload-1", bytes("abc"), "application/gzip", config);

      expect(storagePath).toBe("r2://buc-artifacts/prefix/uploads/upload-1/content.blob");
      expect(requests).toHaveLength(1);
      const request = requests[0]!;
      expect(request.url).toBe(
        "https://account-id.r2.cloudflarestorage.com/buc-artifacts/prefix/uploads/upload-1/content.blob",
      );
      expect(request.init?.method).toBe("PUT");
      const headers = new Headers(request.init?.headers);
      expect(headers.get("content-type")).toBe("application/gzip");
      expect(headers.get("x-amz-content-sha256")).toBe(
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
      );
      expect(headers.get("authorization")).toContain("Credential=access-key/");
      expect(headers.get("authorization")).toContain("/auto/s3/aws4_request");
      expect(headers.get("authorization")).toContain("SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date");
    } finally {
      globalThis.fetch = previousFetch;
    }
  });

  test("reads and materializes objects stored in R2", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-object-storage-"));
    const requests: Array<{ url: string; init: RequestInit | undefined }> = [];
    const previousFetch = globalThis.fetch;
    globalThis.fetch = (async (url: string | URL | Request, init?: RequestInit) => {
      requests.push({ url: String(url), init });
      return new Response("remote package", { status: 200 });
    }) as typeof fetch;
    try {
      const config = r2Config();
      const storagePath = "r2://buc-artifacts/prefix/uploads/upload-1/content.blob";

      expect(await readStoredObject(storagePath, config)).toEqual(bytes("remote package"));
      const localPath = await materializeStoredObject(storagePath, join(root, "import", "archive"), "package.blob", config);

      expect(await readFile(localPath, "utf8")).toBe("remote package");
      expect(requests.map((request) => request.url)).toEqual([
        "https://account-id.r2.cloudflarestorage.com/buc-artifacts/prefix/uploads/upload-1/content.blob",
        "https://account-id.r2.cloudflarestorage.com/buc-artifacts/prefix/uploads/upload-1/content.blob",
      ]);
      expect(requests.every((request) => request.init?.method === "GET")).toBe(true);
    } finally {
      globalThis.fetch = previousFetch;
      await rm(root, { recursive: true, force: true });
    }
  });

  test("rejects local paths when configured for R2 storage", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-object-storage-"));
    const previousFetch = globalThis.fetch;
    let fetchCalls = 0;
    globalThis.fetch = (async () => {
      fetchCalls += 1;
      return new Response("should not fetch", { status: 200 });
    }) as typeof fetch;
    try {
      const secretPath = join(root, "customer-a-prod-openai-token.env");
      await writeFile(secretPath, "secret");

      for (const action of [
        () => readStoredObject(secretPath, r2Config()),
        () => materializeStoredObject(secretPath, join(root, "work"), "package.blob", r2Config()),
      ]) {
        let caught: unknown;
        try {
          await action();
        } catch (error) {
          caught = error;
        }

        expect(caught).toBeInstanceOf(Error);
        const text = JSON.stringify({
          name: (caught as Error).name,
          message: (caught as Error).message,
        });
        expect(text).toContain("configured R2 storage");
        expect(text).not.toContain("customer-a");
        expect(text).not.toContain("openai-token");
      }
      expect(fetchCalls).toBe(0);
    } finally {
      globalThis.fetch = previousFetch;
      await rm(root, { recursive: true, force: true });
    }
  });

  test("rejects R2 objects outside the configured upload storage prefix", async () => {
    const previousFetch = globalThis.fetch;
    let fetchCalls = 0;
    globalThis.fetch = (async () => {
      fetchCalls += 1;
      return new Response("should not fetch", { status: 200 });
    }) as typeof fetch;
    try {
      const storagePath = "r2://buc-artifacts/prefix/customer-a-prod-openai-token.env";
      let caught: unknown;
      try {
        await readStoredObject(storagePath, r2Config());
      } catch (error) {
        caught = error;
      }

      expect(caught).toBeInstanceOf(Error);
      const text = JSON.stringify({
        name: (caught as Error).name,
        message: (caught as Error).message,
      });
      expect(text).toContain("configured R2 upload storage");
      expect(text).not.toContain("customer-a");
      expect(text).not.toContain("openai-token");
      expect(fetchCalls).toBe(0);
    } finally {
      globalThis.fetch = previousFetch;
    }
  });
});

function r2Config() {
  return loadConfig({
    BUCEPHALUS_CLOUD_STORAGE_BACKEND: "r2",
    BUCEPHALUS_CLOUD_R2_ACCOUNT_ID: "account-id",
    BUCEPHALUS_CLOUD_R2_BUCKET: "buc-artifacts",
    BUCEPHALUS_CLOUD_R2_PREFIX: "/prefix/",
    BUCEPHALUS_CLOUD_R2_ACCESS_KEY_ID: "access-key",
    BUCEPHALUS_CLOUD_R2_SECRET_ACCESS_KEY: "secret-key",
  });
}

function bytes(value: string): Uint8Array {
  return new TextEncoder().encode(value);
}

import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, test } from "bun:test";
import { loadConfig } from "../src/config";
import { materializeStoredObject, putRuntimeObject, putUploadObject, readStoredObject } from "../src/objectStorage";

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

  test("stores runtime artifacts on the filesystem by run and trial identity", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-object-storage-"));
    try {
      const config = loadConfig({
        BUCEPHALUS_CLOUD_DATA_DIR: root,
      });
      const storagePath = await putRuntimeObject({
        cloudRunId: "cloud-run-1",
        attemptId: "attempt-1",
        coreRunId: "run_20260614_051159_561175_000001",
        trialId: "trial_1",
        trialAttempt: 0,
        role: "agent_result",
        bytes: bytes("generated answer"),
        mediaType: "application/json",
      }, config);

      expect(storagePath).toBe(join(
        root,
        "runtime-objects",
        "cloud-run-1",
        "attempt-1",
        "run_20260614_051159_561175_000001",
        "trial_1",
        "0",
        "agent_result",
        "content.blob",
      ));
      expect(await readStoredObject(storagePath, config)).toEqual(bytes("generated answer"));
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

  test("writes upload objects to GCS using the Cloud Run service account", async () => {
    const requests: Array<{ url: string; init: RequestInit | undefined }> = [];
    const previousFetch = globalThis.fetch;
    globalThis.fetch = (async (url: string | URL | Request, init?: RequestInit) => {
      requests.push({ url: String(url), init });
      if (String(url).includes("metadata.google.internal")) {
        return Response.json({ access_token: "metadata-token" });
      }
      return new Response(null, { status: 200 });
    }) as typeof fetch;
    try {
      const config = gcsConfig();
      const storagePath = await putUploadObject("upload-1", bytes("abc"), "application/gzip", config);

      expect(storagePath).toBe("gcs://buc-artifacts/prefix/uploads/upload-1/content.blob");
      expect(requests).toHaveLength(2);
      expect(requests[0]!.url).toBe(
        "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token",
      );
      const request = requests[1]!;
      expect(request.url).toBe(
        "https://storage.googleapis.com/upload/storage/v1/b/buc-artifacts/o?uploadType=media&name=prefix%2Fuploads%2Fupload-1%2Fcontent.blob",
      );
      expect(request.init?.method).toBe("POST");
      const headers = new Headers(request.init?.headers);
      expect(headers.get("authorization")).toBe("Bearer metadata-token");
      expect(headers.get("content-type")).toBe("application/gzip");
    } finally {
      globalThis.fetch = previousFetch;
    }
  });

  test("reads and materializes objects stored in GCS", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-object-storage-"));
    const requests: Array<{ url: string; init: RequestInit | undefined }> = [];
    const previousFetch = globalThis.fetch;
    globalThis.fetch = (async (url: string | URL | Request, init?: RequestInit) => {
      requests.push({ url: String(url), init });
      if (String(url).includes("metadata.google.internal")) {
        return Response.json({ access_token: "metadata-token" });
      }
      return new Response("remote package", { status: 200 });
    }) as typeof fetch;
    try {
      const config = gcsConfig();
      const storagePath = "gcs://buc-artifacts/prefix/uploads/upload-1/content.blob";

      expect(await readStoredObject(storagePath, config)).toEqual(bytes("remote package"));
      const localPath = await materializeStoredObject(storagePath, join(root, "import", "archive"), "package.blob", config);

      expect(await readFile(localPath, "utf8")).toBe("remote package");
      expect(requests.map((request) => request.url)).toEqual([
        "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token",
        "https://storage.googleapis.com/storage/v1/b/buc-artifacts/o/prefix%2Fuploads%2Fupload-1%2Fcontent.blob?alt=media",
        "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token",
        "https://storage.googleapis.com/storage/v1/b/buc-artifacts/o/prefix%2Fuploads%2Fupload-1%2Fcontent.blob?alt=media",
      ]);
      expect(requests.filter((request) => !request.url.includes("metadata.google.internal")).every((request) => request.init?.method === "GET")).toBe(true);
    } finally {
      globalThis.fetch = previousFetch;
      await rm(root, { recursive: true, force: true });
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

function gcsConfig() {
  return loadConfig({
    BUCEPHALUS_CLOUD_STORAGE_BACKEND: "gcs",
    BUCEPHALUS_CLOUD_GCS_BUCKET: "buc-artifacts",
    BUCEPHALUS_CLOUD_GCS_PREFIX: "/prefix/",
  });
}

function bytes(value: string): Uint8Array {
  return new TextEncoder().encode(value);
}

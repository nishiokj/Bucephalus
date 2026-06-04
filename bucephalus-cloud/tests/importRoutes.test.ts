import { mkdtemp, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { describe, expect, test } from "bun:test";
import { handleImportRoute } from "../src/routes/imports";
import type { AuthContext } from "../src/auth";
import type { ImportRepository, UploadRecord } from "../src/imports/repository";
import type { PackageRepository } from "../src/packages/repository";

describe("Cloud import routes", () => {
  test("rejects oversized upload declarations before creating an upload", async () => {
    const previous = process.env.BUCEPHALUS_CLOUD_MAX_UPLOAD_BYTES;
    process.env.BUCEPHALUS_CLOUD_MAX_UPLOAD_BYTES = "3";
    try {
      const imports = {
        async createUpload() {
          throw new Error("createUpload should not be called");
        },
      };

      await expect(handleImportRoute(
        jsonRequest("https://cloud.example/v1/uploads", {
          filename: "package.tgz",
          byte_size: 4,
        }),
        new URL("https://cloud.example/v1/uploads"),
        imports as unknown as ImportRepository,
        {} as PackageRepository,
      )).rejects.toThrow("Upload byte_size exceeds");
    } finally {
      restoreEnv("BUCEPHALUS_CLOUD_MAX_UPLOAD_BYTES", previous);
    }
  });

  test("rejects malformed expected upload digests", async () => {
    const imports = {
      async createUpload() {
        throw new Error("createUpload should not be called");
      },
    };

    await expect(handleImportRoute(
      jsonRequest("https://cloud.example/v1/uploads", {
        filename: "package.tgz",
        expected_digest: "sha256:not-hex",
      }),
      new URL("https://cloud.example/v1/uploads"),
      imports as unknown as ImportRepository,
      {} as PackageRepository,
    )).rejects.toThrow("expected_digest must be");
  });

  test("rejects upload bodies that do not match declared byte_size", async () => {
    let markedUploaded = false;
    const imports = {
      async getUpload(): Promise<UploadRecord> {
        return uploadRecord({
          byte_size: 5,
        });
      },
      async markUploaded() {
        markedUploaded = true;
        throw new Error("markUploaded should not be called");
      },
    };

    await expect(handleImportRoute(
      new Request("https://cloud.example/v1/uploads/upload-1/content", {
        method: "PUT",
        body: new Uint8Array([1, 2, 3]),
      }),
      new URL("https://cloud.example/v1/uploads/upload-1/content"),
      imports as unknown as ImportRepository,
      {} as PackageRepository,
    )).rejects.toThrow("byte_size");
    expect(markedUploaded).toBe(false);
  });

  test("stores upload bytes at a server-controlled path instead of the user filename", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-import-route-"));
    const previous = process.env.BUCEPHALUS_CLOUD_DATA_DIR;
    process.env.BUCEPHALUS_CLOUD_DATA_DIR = root;
    try {
      const observed: { storagePath?: string } = {};
      const imports = {
        async getUpload(): Promise<UploadRecord> {
          return uploadRecord({
            filename: "..",
            byte_size: 3,
          });
        },
        async markUploaded(input: { storagePath: string }): Promise<UploadRecord> {
          observed.storagePath = input.storagePath;
          return uploadRecord({
            filename: "..",
            byte_size: 3,
            storage_path: input.storagePath,
            status: "uploaded",
          });
        },
      };

      await handleImportRoute(
        new Request("https://cloud.example/v1/uploads/upload-1/content", {
          method: "PUT",
          body: new Uint8Array([1, 2, 3]),
        }),
        new URL("https://cloud.example/v1/uploads/upload-1/content"),
        imports as unknown as ImportRepository,
        {} as PackageRepository,
      );

      expect(observed.storagePath).toBe(join(root, "uploads", "upload-1", "content.blob"));
    } finally {
      restoreEnv("BUCEPHALUS_CLOUD_DATA_DIR", previous);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("rejects malformed upload content-length headers", async () => {
    const imports = {
      async getUpload(): Promise<UploadRecord> {
        return uploadRecord();
      },
      async markUploaded() {
        throw new Error("markUploaded should not be called");
      },
    };

    await expect(handleImportRoute(
      new Request("https://cloud.example/v1/uploads/upload-1/content", {
        method: "PUT",
        headers: {
          "content-length": "3junk",
        },
        body: new Uint8Array([1, 2, 3]),
      }),
      new URL("https://cloud.example/v1/uploads/upload-1/content"),
      imports as unknown as ImportRepository,
      {} as PackageRepository,
    )).rejects.toThrow("Invalid upload content-length");
  });

  test("creates uploads under the authenticated owner", async () => {
    const observed: { ownerKey?: string | null | undefined } = {};
    const imports = {
      async createUpload(input: { ownerKey?: string | null | undefined }): Promise<UploadRecord> {
        observed.ownerKey = input.ownerKey;
        return uploadRecord();
      },
    };

    await handleImportRoute(
      jsonRequest("https://cloud.example/v1/uploads", {
        filename: "package.tgz",
      }),
      new URL("https://cloud.example/v1/uploads"),
      imports as unknown as ImportRepository,
      {} as PackageRepository,
      authContext("user-a"),
    );

    expect(observed.ownerKey).toBe("issuer:user-a");
  });

  test("lists only import jobs for the authenticated owner", async () => {
    const observed: { ownerKey?: string | undefined } = {};
    const imports = {
      async listImportJobs(input: { ownerKey?: string | undefined }) {
        observed.ownerKey = input.ownerKey;
        return [];
      },
    };

    const response = await handleImportRoute(
      new Request("https://cloud.example/v1/imports"),
      new URL("https://cloud.example/v1/imports"),
      imports as unknown as ImportRepository,
      {} as PackageRepository,
      authContext("user-b"),
    );

    expect(response).not.toBeNull();
    expect(observed.ownerKey).toBe("issuer:user-b");
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

function uploadRecord(overrides: Partial<UploadRecord> = {}): UploadRecord {
  return {
    upload_id: "upload-1",
    filename: "package.tgz",
    media_type: "application/gzip",
    expected_digest: null,
    content_digest: null,
    byte_size: null,
    storage_path: null,
    status: "created",
    created_at: "2026-06-04T00:00:00Z",
    uploaded_at: null,
    completed_at: null,
    error_message: null,
    ...overrides,
  };
}

function restoreEnv(name: string, value: string | undefined): void {
  if (value === undefined) {
    delete process.env[name];
  } else {
    process.env[name] = value;
  }
}

function authContext(subject: string): AuthContext {
  return {
    subject,
    issuer: "issuer",
    audience: "audience",
    claims: {
      sub: subject,
      iss: "issuer",
      aud: "audience",
    },
  };
}

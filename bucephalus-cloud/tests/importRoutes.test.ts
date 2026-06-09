import { mkdir, mkdtemp, readdir, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { describe, expect, test } from "bun:test";
import * as tar from "tar";
import { HttpError } from "../src/http";
import { SealedPackageInspectionError, type ImportDiagnostic } from "../src/imports/sealedPackage";
import { handleImportRoute, publicImportDiagnostics, publicImportInspectionFailure } from "../src/routes/imports";
import type { AuthContext } from "../src/auth";
import type { ImportJobRecord, ImportRepository, UploadRecord } from "../src/imports/repository";
import type { PackageRepository } from "../src/packages/repository";

const UPLOAD_ID = "66666666-6666-4666-8666-666666666666";
const IMPORT_ID = "77777777-7777-4777-8777-777777777777";

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

  test("rejects malformed upload media types before creating an upload", async () => {
    let createCalls = 0;
    const imports = {
      async createUpload() {
        createCalls += 1;
        throw new Error("createUpload should not be called");
      },
    };

    let caught: unknown;
    try {
      await handleImportRoute(
        jsonRequest("https://cloud.example/v1/uploads", {
          filename: "package.tgz",
          media_type: "application/gzip token=raw-upload-media-secret /Users/alice/private",
        }),
        new URL("https://cloud.example/v1/uploads"),
        imports as unknown as ImportRepository,
        {} as PackageRepository,
      );
    } catch (error) {
      caught = error;
    }

    expect(caught).toBeInstanceOf(HttpError);
    const error = caught as HttpError;
    expect(error.status).toBe(400);
    expect(error.code).toBe("invalid_upload_media_type");
    expect(error.message).toBe("media_type must be a MIME type like application/gzip");
    expect(JSON.stringify({ message: error.message, detail: error.detail })).not.toContain("raw-upload-media-secret");
    expect(JSON.stringify({ message: error.message, detail: error.detail })).not.toContain("/Users/alice");
    expect(createCalls).toBe(0);
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
      new Request(`https://cloud.example/v1/uploads/${UPLOAD_ID}/content`, {
        method: "PUT",
        body: new Uint8Array([1, 2, 3]),
      }),
      new URL(`https://cloud.example/v1/uploads/${UPLOAD_ID}/content`),
      imports as unknown as ImportRepository,
      {} as PackageRepository,
    )).rejects.toThrow("byte_size");
    expect(markedUploaded).toBe(false);
  });

  test("rejects invalid persisted upload media types before storing content", async () => {
    let markedUploaded = false;
    const imports = {
      async getUpload(): Promise<UploadRecord> {
        return uploadRecord({
          byte_size: 3,
          media_type: "application/gzip token=raw-persisted-media-secret",
        });
      },
      async markUploaded() {
        markedUploaded = true;
        throw new Error("markUploaded should not be called");
      },
    };

    let caught: unknown;
    try {
      await handleImportRoute(
        new Request(`https://cloud.example/v1/uploads/${UPLOAD_ID}/content`, {
          method: "PUT",
          body: new Uint8Array([1, 2, 3]),
        }),
        new URL(`https://cloud.example/v1/uploads/${UPLOAD_ID}/content`),
        imports as unknown as ImportRepository,
        {} as PackageRepository,
      );
    } catch (error) {
      caught = error;
    }

    expect(caught).toBeInstanceOf(HttpError);
    const error = caught as HttpError;
    expect(error.status).toBe(500);
    expect(error.code).toBe("invalid_persisted_upload_media_type");
    const encoded = JSON.stringify({ message: error.message, detail: error.detail });
    expect(encoded).not.toContain("raw-persisted-media-secret");
    expect(encoded).not.toContain("application/gzip token");
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
        new Request(`https://cloud.example/v1/uploads/${UPLOAD_ID}/content`, {
          method: "PUT",
          body: new Uint8Array([1, 2, 3]),
        }),
        new URL(`https://cloud.example/v1/uploads/${UPLOAD_ID}/content`),
        imports as unknown as ImportRepository,
        {} as PackageRepository,
      );

      expect(observed.storagePath).toBe(join(root, "uploads", UPLOAD_ID, "content.blob"));
    } finally {
      restoreEnv("BUCEPHALUS_CLOUD_DATA_DIR", previous);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("accepts persisted bigint upload sizes when comparing content-length", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-import-route-"));
    const previous = process.env.BUCEPHALUS_CLOUD_DATA_DIR;
    process.env.BUCEPHALUS_CLOUD_DATA_DIR = root;
    try {
      let markedUploaded = false;
      const imports = {
        async getUpload(): Promise<UploadRecord> {
          return uploadRecord({
            byte_size: "3" as unknown as number,
          });
        },
        async markUploaded(input: { byteSize: number }): Promise<UploadRecord> {
          markedUploaded = true;
          expect(input.byteSize).toBe(3);
          return uploadRecord({
            byte_size: input.byteSize,
            status: "uploaded",
          });
        },
      };

      const response = await handleImportRoute(
        new Request(`https://cloud.example/v1/uploads/${UPLOAD_ID}/content`, {
          method: "PUT",
          headers: {
            "content-length": "3",
          },
          body: new Uint8Array([1, 2, 3]),
        }),
        new URL(`https://cloud.example/v1/uploads/${UPLOAD_ID}/content`),
        imports as unknown as ImportRepository,
        {} as PackageRepository,
      );

      expect(markedUploaded).toBe(true);
      expect(await response?.json()).toMatchObject({
        byte_size: 3,
        status: "uploaded",
      });
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
      new Request(`https://cloud.example/v1/uploads/${UPLOAD_ID}/content`, {
        method: "PUT",
        headers: {
          "content-length": "3junk",
        },
        body: new Uint8Array([1, 2, 3]),
      }),
      new URL(`https://cloud.example/v1/uploads/${UPLOAD_ID}/content`),
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

  test("rejects invalid import list limits before listing", async () => {
    let listCalls = 0;
    const imports = {
      async listImportJobs() {
        listCalls += 1;
        return [];
      },
    };

    await expect(handleImportRoute(
      new Request("https://cloud.example/v1/imports?limit=token=raw-import-limit"),
      new URL("https://cloud.example/v1/imports?limit=token=raw-import-limit"),
      imports as unknown as ImportRepository,
      {} as PackageRepository,
      authContext("user-b"),
    )).rejects.toThrow("limit must be an integer from 1 to 200");
    expect(listCalls).toBe(0);
  });

  test("upload paths fail clearly on malformed encoding before repository calls", async () => {
    let importCalls = 0;
    const imports = {
      async getUpload() {
        importCalls += 1;
        return null;
      },
      async markUploaded() {
        importCalls += 1;
        return uploadRecord();
      },
    };

    let caught: unknown;
    try {
      await handleImportRoute(
        new Request("https://cloud.example/v1/uploads/%E0%A4%A-token=raw-upload-path-secret/content", {
          method: "PUT",
          body: new Uint8Array([1, 2, 3]),
        }),
        new URL("https://cloud.example/v1/uploads/%E0%A4%A-token=raw-upload-path-secret/content"),
        imports as unknown as ImportRepository,
        {} as PackageRepository,
      );
    } catch (error) {
      caught = error;
    }

    expect(caught).toBeInstanceOf(HttpError);
    const error = caught as HttpError;
    expect(error.status).toBe(400);
    expect(error.code).toBe("invalid_path_param");
    expect(error.message).toBe("/upload_id must be valid percent-encoded UTF-8");
    expect(JSON.stringify({ message: error.message, detail: error.detail })).not.toContain("raw-upload-path-secret");
    expect(importCalls).toBe(0);
  });

  test("upload and import ids reject malformed UUIDs before repository calls", async () => {
    const calls: string[] = [];
    const imports = {
      async getUpload() {
        calls.push("getUpload");
        return uploadRecord();
      },
      async markUploaded() {
        calls.push("markUploaded");
        return uploadRecord();
      },
      async completeUpload() {
        calls.push("completeUpload");
        return uploadRecord();
      },
      async createImportJob() {
        calls.push("createImportJob");
        return IMPORT_ID;
      },
      async getImportJob() {
        calls.push("getImportJob");
        return importJobRecord();
      },
    };

    const cases: Array<{
      request: Request;
      url: URL;
      message: string;
      forbidden: string;
    }> = [
      {
        request: new Request("https://cloud.example/v1/uploads/token=raw-upload-id-secret/content", {
          method: "PUT",
          body: new Uint8Array([1, 2, 3]),
        }),
        url: new URL("https://cloud.example/v1/uploads/token=raw-upload-id-secret/content"),
        message: "/upload_id must be a UUID",
        forbidden: "raw-upload-id-secret",
      },
      {
        request: new Request("https://cloud.example/v1/uploads/not-a-uuid/complete", {
          method: "POST",
        }),
        url: new URL("https://cloud.example/v1/uploads/not-a-uuid/complete"),
        message: "/upload_id must be a UUID",
        forbidden: "not-a-uuid",
      },
      {
        request: jsonRequest("https://cloud.example/v1/imports/sealed-package", {
          upload_id: "token=raw-import-upload-secret /Users/alice/private/upload",
        }),
        url: new URL("https://cloud.example/v1/imports/sealed-package"),
        message: "/upload_id must be a UUID",
        forbidden: "raw-import-upload-secret",
      },
      {
        request: new Request("https://cloud.example/v1/imports/token=raw-import-id-secret"),
        url: new URL("https://cloud.example/v1/imports/token=raw-import-id-secret"),
        message: "/import_id must be a UUID",
        forbidden: "raw-import-id-secret",
      },
    ];

    for (const testCase of cases) {
      let caught: unknown;
      try {
        await handleImportRoute(
          testCase.request,
          testCase.url,
          imports as unknown as ImportRepository,
          {} as PackageRepository,
          authContext("user-a"),
        );
      } catch (error) {
        caught = error;
      }

      expect(caught).toBeInstanceOf(HttpError);
      const error = caught as HttpError;
      expect(error.status).toBe(400);
      expect(error.code).toBe("invalid_request");
      expect(error.message).toBe(testCase.message);
      const encoded = JSON.stringify({ message: error.message, detail: error.detail });
      expect(encoded).not.toContain(testCase.forbidden);
      expect(encoded).not.toContain("/Users/alice");
    }
    expect(calls).toEqual([]);
  });

  test("cleans import inspection work dirs after failed sealed package imports", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-import-route-"));
    const previous = process.env.BUCEPHALUS_CLOUD_DATA_DIR;
    process.env.BUCEPHALUS_CLOUD_DATA_DIR = root;
    try {
      const storagePath = await writeMalformedStoredPackage(root, UPLOAD_ID);
      const updates: Array<{ status: string; errorMessage?: string | null; diagnostics?: ImportDiagnostic[] }> = [];
      const imports = {
        async getUpload(): Promise<UploadRecord> {
          return uploadRecord({
            status: "completed",
            storage_path: storagePath,
            byte_size: 1,
          });
        },
        async createImportJob(): Promise<string> {
          return IMPORT_ID;
        },
        async updateImportInspection(input: {
          status: string;
          errorMessage?: string | null;
          diagnostics?: ImportDiagnostic[];
        }) {
          updates.push(input);
        },
        async getImportJob(): Promise<ImportJobRecord> {
          return importJobRecord({
            status: updates.at(-1)?.status ?? "failed",
            error_message: updates.at(-1)?.errorMessage ?? null,
            diagnostics: updates.at(-1)?.diagnostics ?? [],
          });
        },
      };
      const packages = {
        async upsertArtifact() {
          throw new Error("upsertArtifact should not be called for failed imports");
        },
      };

      const response = await handleImportRoute(
        jsonRequest("https://cloud.example/v1/imports/sealed-package", {
          upload_id: UPLOAD_ID,
        }),
        new URL("https://cloud.example/v1/imports/sealed-package"),
        imports as unknown as ImportRepository,
        packages as unknown as PackageRepository,
        authContext("user-a"),
      );

      expect(response).not.toBeNull();
      const body = await response!.json();
      const encoded = JSON.stringify(body);
      expect(body.status).toBe("failed");
      expect(updates.at(-1)?.status).toBe("failed");
      expect(await importWorkspaceEntries(root)).not.toContain(IMPORT_ID);
      expect(encoded).not.toContain("customer-a");
      expect(encoded).not.toContain("openai-token");
    } finally {
      restoreEnv("BUCEPHALUS_CLOUD_DATA_DIR", previous);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("redacts import job errors and diagnostics in user-facing responses", async () => {
    const imports = {
      async listImportJobs() {
        return [importJobRecord({
          label: "Imported from /Users/alice/private/package.tgz token=raw-import-label",
          error_message:
            "sealed package archive contains unsafe entry path '/Users/alice/private/package token=raw-import-token'",
          diagnostics: [{
            severity: "error",
            code: "unsafe_archive_path",
            pointer: "/Users/alice/private/package",
            message:
              "Archive entry '/Users/alice/private/package?token=raw-query' from file:///private/tmp/package.tgz is unsafe; api_key=sk-abcdefghijklmnopqrstuvwxyz",
          }],
        })];
      },
    };

    const response = await handleImportRoute(
      new Request("https://cloud.example/v1/imports"),
      new URL("https://cloud.example/v1/imports"),
      imports as unknown as ImportRepository,
      {} as PackageRepository,
    );

    expect(response).not.toBeNull();
    const body = await response!.json();
    const text = JSON.stringify(body);

    expect(body.imports[0].label).toContain("[redacted-local-path]");
    expect(body.imports[0].label).toContain("token=[redacted-secret]");
    expect(body.imports[0].error_message).toContain("[redacted-local-path]");
    expect(body.imports[0].error_message).toContain("token=[redacted-secret]");
    expect(body.imports[0].diagnostics[0].pointer).toBe("[redacted-local-path]");
    expect(body.imports[0].diagnostics[0].message).toContain("file://[redacted-local-path]");
    expect(body.imports[0].diagnostics[0].message).toContain("api_key=[redacted-secret]");
    expect(text).not.toContain("/Users/alice");
    expect(text).not.toContain("/private/tmp");
    expect(text).not.toContain("raw-import-label");
    expect(text).not.toContain("raw-import-token");
    expect(text).not.toContain("raw-query");
    expect(text).not.toContain("sk-abcdefghijklmnopqrstuvwxyz");
  });

  test("sanitizes import inspection failures before persistence", () => {
    const failure = publicImportInspectionFailure(new SealedPackageInspectionError(
      "sealed package archive contains unsafe entry path '/Users/alice/private/package token=raw-import-token'",
      [{
        severity: "error",
        code: "unsafe_archive_path",
        pointer: "/Users/alice/private/package",
        message:
          "Archive entry '/Users/alice/private/package?token=raw-query' from file:///private/tmp/package.tgz is unsafe; api_key=sk-abcdefghijklmnopqrstuvwxyz",
      }],
    ));
    const text = JSON.stringify(failure);
    const diagnostic = failure.diagnostics.at(0);
    if (!diagnostic) {
      throw new Error("expected sanitized import diagnostic");
    }

    expect(failure.errorMessage).toContain("[redacted-local-path]");
    expect(failure.errorMessage).toContain("token=[redacted-secret]");
    expect(diagnostic.pointer).toBe("[redacted-local-path]");
    expect(diagnostic.message).toContain("file://[redacted-local-path]");
    expect(diagnostic.message).toContain("api_key=[redacted-secret]");
    expect(text).not.toContain("/Users/alice");
    expect(text).not.toContain("/private/tmp");
    expect(text).not.toContain("raw-import-token");
    expect(text).not.toContain("raw-query");
    expect(text).not.toContain("sk-abcdefghijklmnopqrstuvwxyz");
  });

  test("sanitizes accepted import diagnostics before artifact persistence", () => {
    const diagnostics: ImportDiagnostic[] = [{
      severity: "warning",
      code: "packaged_secret_hint",
      pointer: "/resolved_experiment/files/%USERPROFILE%/credentials",
      message:
        "Resolved package file /home/alice/project/secrets.env carries bearer_token=ya29.abcdefghijklmnopqrstuvwxyz123456",
    }];

    const sanitized = publicImportDiagnostics(diagnostics);
    const text = JSON.stringify(sanitized);
    const diagnostic = sanitized.at(0);
    if (!diagnostic) {
      throw new Error("expected sanitized accepted import diagnostic");
    }

    expect(diagnostic.pointer).toContain("[redacted-local-path]");
    expect(diagnostic.message).toContain("[redacted-local-path]");
    expect(diagnostic.message).toContain("bearer_token=[redacted-secret]");
    expect(text).not.toContain("%USERPROFILE%");
    expect(text).not.toContain("/home/alice");
    expect(text).not.toContain("ya29.abcdefghijklmnopqrstuvwxyz123456");
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
    upload_id: UPLOAD_ID,
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

function importJobRecord(overrides: Partial<ImportJobRecord> = {}): ImportJobRecord {
  return {
    import_id: IMPORT_ID,
    upload_id: UPLOAD_ID,
    import_type: "sealed_package",
    status: "failed",
    label: null,
    package_digest: null,
    manifest_json: null,
    resolved_experiment_json: null,
    created_at: "2026-06-04T00:00:00Z",
    updated_at: "2026-06-04T00:00:00Z",
    error_message: null,
    diagnostics: [],
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

async function writeMalformedStoredPackage(root: string, uploadId: string): Promise<string> {
  const uploadDir = join(root, "uploads", uploadId);
  await mkdir(uploadDir, { recursive: true });
  const packageDir = join(root, "malformed-package");
  await mkdir(join(packageDir, "files"), { recursive: true });
  await writeFile(join(packageDir, "manifest.json"), JSON.stringify({
    schema_version: "sealed_run_package_v2",
    created_at: "2026-06-04T00:00:00Z",
    resolved_experiment: currentResolvedExperiment(),
    checksums_ref: "checksums.json",
    package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  }));
  await writeFile(join(packageDir, "files", "customer-a-prod-openai-token.env"), "secret");
  const storagePath = join(uploadDir, "content.blob");
  await tar.c({ gzip: true, cwd: packageDir, file: storagePath }, (await readdir(packageDir)).sort());
  return storagePath;
}

async function importWorkspaceEntries(root: string): Promise<string[]> {
  try {
    return await readdir(join(root, "imports"));
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") {
      return [];
    }
    throw error;
  }
}

function currentResolvedExperiment() {
  return {
    runtime: {
      compute: { backend: "local-docker" },
      secrets: [],
    },
  };
}

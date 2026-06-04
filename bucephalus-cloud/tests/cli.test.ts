import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { spawn } from "node:child_process";
import * as tar from "tar";
import { describe, expect, test } from "bun:test";

const cliPath = join(import.meta.dir, "../src/cli.ts");

describe("Cloud CLI deploy", () => {
  test("builds with Core, archives the sealed package, uploads it, and imports it", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-cloud-cli-"));
    const requests: Array<{ method: string; path: string; body: unknown; auth: string | null }> = [];
    let uploadedBytes = Buffer.alloc(0);
    const server = Bun.serve({
      port: 0,
      async fetch(request) {
        const url = new URL(request.url);
        const auth = request.headers.get("authorization");
        if (url.pathname === "/v1/uploads" && request.method === "POST") {
          requests.push({ method: request.method, path: url.pathname, body: await request.json(), auth });
          return Response.json({ upload_id: "upload-1" });
        }
        if (url.pathname === "/v1/uploads/upload-1/content" && request.method === "PUT") {
          uploadedBytes = Buffer.from(await request.arrayBuffer());
          requests.push({ method: request.method, path: url.pathname, body: { byte_size: uploadedBytes.byteLength }, auth });
          return Response.json({ ok: true });
        }
        if (url.pathname === "/v1/uploads/upload-1/complete" && request.method === "POST") {
          requests.push({ method: request.method, path: url.pathname, body: await request.json(), auth });
          return Response.json({ status: "completed" });
        }
        if (url.pathname === "/v1/imports/sealed-package" && request.method === "POST") {
          requests.push({ method: request.method, path: url.pathname, body: await request.json(), auth });
          return Response.json({ import_id: "import-1", status: "accepted", package_digest: "sha256:package" });
        }
        return new Response("not found", { status: 404 });
      },
    });

    try {
      const fakeCore = join(root, "fake-bucephalus");
      await writeFile(fakeCore, fakeCoreScript());
      await chmod(fakeCore, 0o755);
      const experiment = join(root, "experiment.yaml");
      await writeFile(experiment, "experiment:\n  id: smoke\n");
      const packageDir = join(root, "package");
      const archivePath = join(root, "package.tgz");

      const result = await runCli([
        "--api-url", `http://127.0.0.1:${server.port}`,
        "--user-token", "user-token",
        "deploy", experiment,
        "--label", "smoke",
        "--out", packageDir,
        "--archive-out", archivePath,
        "--core-cmd", fakeCore,
      ]);

      expect(result.exitCode).toBe(0);
      expect(JSON.parse(result.stdout)).toEqual({
        import_id: "import-1",
        status: "accepted",
        package_digest: "sha256:package",
        package_dir: packageDir,
        archive_path: archivePath,
      });
      expect(requests.map((request) => `${request.method} ${request.path}`)).toEqual([
        "POST /v1/uploads",
        "PUT /v1/uploads/upload-1/content",
        "POST /v1/uploads/upload-1/complete",
        "POST /v1/imports/sealed-package",
      ]);
      expect(requests.every((request) => request.auth === "Bearer user-token")).toBe(true);
      const createUploadRequest = requests[0];
      const importRequest = requests[3];
      if (!createUploadRequest || !importRequest) {
        throw new Error("deploy did not issue the expected Cloud API requests");
      }
      expect(createUploadRequest.body).toMatchObject({
        filename: "package.tgz",
        media_type: "application/gzip",
        expected_digest: expect.stringMatching(/^sha256:[0-9a-f]{64}$/),
      });
      expect(importRequest.body).toEqual({ upload_id: "upload-1", label: "smoke" });

      const uploadedArchive = join(root, "uploaded.tgz");
      const extracted = join(root, "uploaded");
      await writeFile(uploadedArchive, uploadedBytes);
      await mkdir(extracted, { recursive: true });
      await tar.x({ file: uploadedArchive, cwd: extracted });
      expect(JSON.parse(await readFile(join(extracted, "manifest.json"), "utf8"))).toMatchObject({
        schema_version: "sealed_run_package_v2",
        package_digest: "sha256:package",
      });
      expect(await readFile(join(extracted, "tasks/tasks.jsonl"), "utf8")).toContain("case-1");
    } finally {
      server.stop(true);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("prints package secret requirements with a safe refs-file workflow", async () => {
    const server = Bun.serve({
      port: 0,
      fetch(request) {
        const url = new URL(request.url);
        if (url.pathname.startsWith("/v1/packages/")) {
          return Response.json(packageWithSecrets());
        }
        return new Response("not found", { status: 404 });
      },
    });
    try {
      const result = await runCli([
        "--api-url", `http://127.0.0.1:${server.port}`,
        "--user-token", "user-token",
        "package", "secrets",
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      ]);

      expect(result.exitCode).toBe(0);
      expect(result.stdout).toContain("OPENAI_API_KEY -> /run/secrets/openai");
      expect(result.stdout).toContain("--secret-ref-file secrets.yaml");
      expect(result.stdout).not.toContain("sk-");
    } finally {
      server.stop(true);
    }
  });

  test("preflights run secrets before queueing a package run", async () => {
    const requests: string[] = [];
    const server = Bun.serve({
      port: 0,
      fetch(request) {
        const url = new URL(request.url);
        requests.push(`${request.method} ${url.pathname}`);
        if (url.pathname.startsWith("/v1/packages/")) {
          return Response.json(packageWithSecrets());
        }
        if (url.pathname === "/v1/runs") {
          return Response.json({ run_id: "run-1" });
        }
        return new Response("not found", { status: 404 });
      },
    });
    try {
      const result = await runCli([
        "--api-url", `http://127.0.0.1:${server.port}`,
        "--user-token", "user-token",
        "run", "create",
        "--package-digest", "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      ]);

      expect(result.exitCode).toBe(1);
      expect(result.stderr).toContain("Missing: OPENAI_API_KEY");
      expect(requests).toEqual([
        "GET /v1/packages/sha256%3Aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      ]);

      const unsupported = await runCli([
        "--api-url", `http://127.0.0.1:${server.port}`,
        "--user-token", "user-token",
        "run", "create",
        "--package-digest", "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "--secret-ref", "OPENAI_API_KEY=raw-openai-key",
      ]);

      expect(unsupported.exitCode).toBe(1);
      expect(unsupported.stderr).toContain("Unsupported ref format: OPENAI_API_KEY");
      expect(requests).toEqual([
        "GET /v1/packages/sha256%3Aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "GET /v1/packages/sha256%3Aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      ]);
    } finally {
      server.stop(true);
    }
  });
});

function packageWithSecrets() {
  return {
    package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    secret_requirements: [
      {
        id: "OPENAI_API_KEY",
        target: "/run/secrets/openai",
        required_for_variants: [],
      },
    ],
  };
}

function fakeCoreScript(): string {
  return `#!/usr/bin/env bun
import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
const outIndex = process.argv.indexOf("--out");
if (process.argv[2] !== "build" || outIndex === -1 || !process.argv[outIndex + 1]) {
  console.error("unexpected args", JSON.stringify(process.argv.slice(2)));
  process.exit(2);
}
const out = process.argv[outIndex + 1];
await mkdir(join(out, "tasks"), { recursive: true });
await writeFile(join(out, "manifest.json"), JSON.stringify({ schema_version: "sealed_run_package_v2", created_at: "2026-06-02T00:00:00Z", resolved_experiment: { experiment: { id: "smoke" } }, checksums_ref: "checksums.json", package_digest: "sha256:package" }));
await writeFile(join(out, "resolved_experiment.json"), JSON.stringify({ experiment: { id: "smoke" } }));
await writeFile(join(out, "checksums.json"), JSON.stringify({ schema_version: "sealed_package_checksums_v2", files: {} }));
await writeFile(join(out, "tasks/tasks.jsonl"), JSON.stringify({ id: "case-1" }) + "\\n");
console.log(JSON.stringify({ ok: true }));
`;
}

async function runCli(args: string[]): Promise<{ exitCode: number; stdout: string; stderr: string }> {
  const { promise, resolve, reject } = Promise.withResolvers<{ exitCode: number; stdout: string; stderr: string }>();
  const child = spawn("bun", ["run", cliPath, ...args], { stdio: ["ignore", "pipe", "pipe"] });
  const stdout: Buffer[] = [];
  const stderr: Buffer[] = [];
  child.stdout.on("data", (chunk) => stdout.push(Buffer.from(chunk)));
  child.stderr.on("data", (chunk) => stderr.push(Buffer.from(chunk)));
  child.on("error", reject);
  child.on("close", (exitCode) => {
    resolve({
      exitCode: exitCode ?? 1,
      stdout: Buffer.concat(stdout).toString("utf8"),
      stderr: Buffer.concat(stderr).toString("utf8"),
    });
  });
  return promise;
}

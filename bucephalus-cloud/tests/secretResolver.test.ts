import { mkdtemp, readFile, rm, stat, symlink } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { describe, expect, test } from "bun:test";
import {
  resolveSecrets,
  secretFetchPlan,
} from "../src/secretResolver";

describe("attempt secret resolver", () => {
  test("plans GCP Secret Manager refs as provider CLI access", () => {
    expect(secretFetchPlan(
      "gcp-secret-manager://projects/acme-prod/secrets/openai_api_key/versions/latest",
      {},
    )).toEqual({
      kind: "command",
      executable: "gcloud",
      args: [
        "secrets",
        "versions",
        "access",
        "latest",
        "--secret",
        "openai_api_key",
        "--project",
        "acme-prod",
      ],
    });
  });

  test("plans GCP Secret Manager refs as metadata access for cloud workers", () => {
    expect(secretFetchPlan(
      "gcp-secret-manager://projects/acme-prod/secrets/openai_api_key/versions/latest",
      { BUCEPHALUS_SECRET_RESOLVER_GCP_AUTH: "metadata" },
    )).toEqual({
      kind: "gcp-metadata",
      project: "acme-prod",
      secret: "openai_api_key",
      version: "latest",
    });
  });

  test("rejects file: refs unless explicitly enabled for local development", () => {
    expect(() => secretFetchPlan("file:/data/secrets/buc-abc", {}))
      .toThrow("file: secret refs are disabled");
  });

  test("plans file: refs as local reads when explicitly enabled", () => {
    expect(secretFetchPlan("file:/data/secrets/buc-abc", {
      BUCEPHALUS_SECRET_RESOLVER_ALLOW_FILE: "true",
    })).toEqual({ kind: "file", path: "/data/secrets/buc-abc" });
  });

  test("rejects relative file: refs", () => {
    expect(() => secretFetchPlan("file:secrets/buc-abc", {
      BUCEPHALUS_SECRET_RESOLVER_ALLOW_FILE: "true",
    })).toThrow("absolute path");
  });

  test("plans AWS Secrets Manager refs as provider CLI access", () => {
    expect(secretFetchPlan(
      "aws-secrets-manager://prod/openai-api-key",
      { BUCEPHALUS_SECRET_RESOLVER_AWS_CMD: "/usr/local/bin/aws" },
    )).toEqual({
      kind: "command",
      executable: "/usr/local/bin/aws",
      args: [
        "secretsmanager",
        "get-secret-value",
        "--secret-id",
        "prod/openai-api-key",
        "--query",
        "SecretString",
        "--output",
        "text",
      ],
    });
  });

  test("rejects env refs unless explicitly enabled for local development", () => {
    expect(() => secretFetchPlan("env:OPENAI_API_KEY", {})).toThrow("env: secret refs are disabled");
  });

  test("rejects Cloud control-plane secret refs by default", () => {
    expect(() => secretFetchPlan(
      "gcp-secret-manager://projects/acme-prod/secrets/buc-prod-worker-token/versions/latest",
      {},
    )).toThrow("reserved Cloud control-plane secret name");

    expect(() => secretFetchPlan(
      "env:BUCEPHALUS_CLOUD_WORKER_TOKEN",
      {
        BUCEPHALUS_SECRET_RESOLVER_ALLOW_ENV: "true",
      },
    )).toThrow("reserved Cloud control-plane environment variable");
  });

  test("can explicitly allow control-plane-looking refs for operator migration", () => {
    expect(secretFetchPlan(
      "gcp-secret-manager://projects/acme-prod/secrets/buc-prod-worker-token/versions/1",
      {
        BUCEPHALUS_SECRET_RESOLVER_ALLOW_CONTROL_PLANE_REFS: "true",
      },
    )).toEqual({
      kind: "command",
      executable: "gcloud",
      args: [
        "secrets",
        "versions",
        "access",
        "1",
        "--secret",
        "buc-prod-worker-token",
        "--project",
        "acme-prod",
      ],
    });
  });

  test("materializes declared secrets under output_dir", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-secret-resolver-"));
    try {
      const output = await resolveSecrets({
        attempt_id: "attempt-1",
        run_id: "run-1",
        output_dir: root,
        secrets: [
          { id: "OPENAI_API_KEY", ref: "env:OPENAI_API_KEY" },
        ],
      }, {
        env: {
          BUCEPHALUS_SECRET_RESOLVER_ALLOW_ENV: "true",
          OPENAI_API_KEY: "secret-value",
        },
        runCommand: async () => {
          throw new Error("provider command should not run for env refs");
        },
      });

      expect(output).toEqual({
        files: {
          OPENAI_API_KEY: "OPENAI_API_KEY.secret",
        },
      });
      const relativePath = output.files.OPENAI_API_KEY;
      if (!relativePath) {
        throw new Error("resolver did not return OPENAI_API_KEY");
      }
      expect(relativePath).toBe("OPENAI_API_KEY.secret");
      const secretPath = join(root, relativePath);
      expect(await readFile(secretPath, "utf8")).toBe("secret-value");
      expect((await stat(secretPath)).isFile()).toBe(true);
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("materializes provider command output without leaking absolute paths in response", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-secret-resolver-"));
    try {
      const output = await resolveSecrets({
        attempt_id: "attempt-1",
        run_id: "run-1",
        output_dir: root,
        secrets: [
          {
            id: "OPENAI_API_KEY",
            ref: "gcp-secret-manager://projects/acme-prod/secrets/openai_api_key/versions/7",
          },
        ],
      }, {
        env: {},
        runCommand: async (executable, args) => {
          expect(executable).toBe("gcloud");
          expect(args).toContain("7");
          return {
            stdout: "provider-secret\n",
            stderr: "",
          };
        },
      });

      const relativePath = output.files.OPENAI_API_KEY;
      if (!relativePath) {
        throw new Error("resolver did not return OPENAI_API_KEY");
      }
      expect(relativePath).toBe("OPENAI_API_KEY.secret");
      expect(JSON.stringify(output)).not.toContain(root);
      expect(await readFile(join(root, relativePath), "utf8")).toBe("provider-secret");
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("materializes GCP metadata secret output without provider command", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-secret-resolver-"));
    const urls: string[] = [];
    try {
      const output = await resolveSecrets({
        attempt_id: "attempt-1",
        run_id: "run-1",
        output_dir: root,
        secrets: [
          {
            id: "OPENAI_API_KEY",
            ref: "gcp-secret-manager://projects/acme-prod/secrets/openai_api_key/versions/7",
          },
        ],
      }, {
        env: {
          BUCEPHALUS_SECRET_RESOLVER_GCP_AUTH: "metadata",
        },
        fetch: async (url) => {
          const href = String(url);
          urls.push(href);
          if (href.includes("metadata.google.internal")) {
            return new Response(JSON.stringify({ access_token: "metadata-token" }));
          }
          expect(href).toContain("projects/acme-prod/secrets/openai_api_key/versions/7:access");
          return new Response(JSON.stringify({
            payload: {
              data: Buffer.from("metadata-secret", "utf8").toString("base64"),
            },
          }));
        },
        runCommand: async () => {
          throw new Error("provider command should not run for metadata refs");
        },
      });

      const relativePath = output.files.OPENAI_API_KEY;
      if (!relativePath) {
        throw new Error("resolver did not return OPENAI_API_KEY");
      }
      expect(urls).toHaveLength(2);
      expect(await readFile(join(root, relativePath), "utf8")).toBe("metadata-secret");
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("refuses to follow a preexisting secret-file symlink out of output_dir", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-secret-resolver-"));
    const outside = await mkdtemp(join(tmpdir(), "buc-secret-outside-"));
    try {
      const outsideTarget = join(outside, "leaked-secret");
      await symlink(outsideTarget, join(root, "OPENAI_API_KEY.secret"));

      await expect(resolveSecrets({
        attempt_id: "attempt-1",
        run_id: "run-1",
        output_dir: root,
        secrets: [
          { id: "OPENAI_API_KEY", ref: "env:OPENAI_API_KEY" },
        ],
      }, {
        env: {
          BUCEPHALUS_SECRET_RESOLVER_ALLOW_ENV: "true",
          OPENAI_API_KEY: "secret-value",
        },
        runCommand: async () => {
          throw new Error("provider command should not run for env refs");
        },
      })).rejects.toThrow("already exists");

      await expect(readFile(outsideTarget, "utf8")).rejects.toThrow();
    } finally {
      await rm(root, { recursive: true, force: true });
      await rm(outside, { recursive: true, force: true });
    }
  });
});

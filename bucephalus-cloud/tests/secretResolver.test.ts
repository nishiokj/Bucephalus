import { mkdtemp, readFile, rm, stat, symlink } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { describe, expect, test } from "bun:test";
import {
  fetchSecretValue,
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

  test("refuses to follow a preexisting secret-file symlink out of output_dir", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-secret-resolver-"));
    const outside = await mkdtemp(join(tmpdir(), "buc-secret-outside-"));
    try {
      const outsideTarget = join(outside, "leaked-secret");
      await symlink(outsideTarget, join(root, "OPENAI_API_KEY.secret"));

      let message = "";
      try {
        await resolveSecrets({
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
        throw new Error("resolver unexpectedly followed preexisting secret-file symlink");
      } catch (error) {
        message = error instanceof Error ? error.message : String(error);
      }

      expect(message).toContain("Secret output file already exists");
      expect(message).toContain("output_ref: secret-output://OPENAI_API_KEY.secret");
      expect(message).not.toContain(root);
      expect(message).not.toContain(outside);
      expect(message).not.toContain(outsideTarget);

      await expect(readFile(outsideTarget, "utf8")).rejects.toThrow();
    } finally {
      await rm(root, { recursive: true, force: true });
      await rm(outside, { recursive: true, force: true });
    }
  });

  test("redacts provider command path and stderr on secret fetch failures", async () => {
    let message = "";
    try {
      await fetchSecretValue(
        "gcp-secret-manager://projects/acme-prod/secrets/openai_api_key/versions/7",
        {
          env: {
            BUCEPHALUS_SECRET_RESOLVER_GCLOUD_CMD: "/Users/alice/private/bin/gcloud",
          },
          runCommand: async () => {
            throw new Error(
              "failed near /Users/alice/private/project token=raw-provider-token "
                + "gcp-secret-manager://projects/acme-prod/secrets/openai_api_key/versions/7 "
                + "https://alice:secret@example.com/result?token=raw-query",
            );
          },
        },
      );
      throw new Error("provider command unexpectedly succeeded");
    } catch (error) {
      message = error instanceof Error ? error.message : String(error);
    }

    expect(message).toContain("Secret provider command failed");
    expect(message).toContain("[redacted-local-path]");
    expect(message).toContain("token=[redacted-secret]");
    expect(message).toContain("[redacted-secret]");
    expect(message).toContain("https://example.com/result [redacted URL credentials/query]");
    expect(message).not.toContain("/Users/alice");
    expect(message).not.toContain("private/bin/gcloud");
    expect(message).not.toContain("raw-provider-token");
    expect(message).not.toContain("raw-query");
    expect(message).not.toContain("gcp-secret-manager://");
  });
});

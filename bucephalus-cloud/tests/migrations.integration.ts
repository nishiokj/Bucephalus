import { describe, expect, test } from "bun:test";
import { createSql } from "../src/db/client";
import type { Sql } from "../src/db/client";
import { migrationFiles, runMigrations } from "../src/db/migrate";
import { PackageRepository, RunRepository, type RunRequirements, type WorkerCapabilities } from "../src/packages/repository";
import { RunnerRepository } from "../src/runners/repository";

const defaultDatabaseUrl = "postgres://bucephalus:bucephalus_dev@127.0.0.1:55432/bucephalus_cloud";

function migrationTestBaseUrl(): string {
  return process.env.BUCEPHALUS_MIGRATION_TEST_DATABASE_URL
    ?? process.env.DATABASE_URL
    ?? defaultDatabaseUrl;
}

function requireSafeDatabaseServer(databaseUrl: string): void {
  const parsed = new URL(databaseUrl);
  if (parsed.protocol !== "postgres:" && parsed.protocol !== "postgresql:") {
    throw new Error(`Migration tests require a postgres URL, got: ${parsed.protocol}`);
  }

  const host = parsed.hostname.toLowerCase();
  const isLocal = host === "localhost" || host === "127.0.0.1" || host === "::1";
  if (!isLocal && process.env.BUCEPHALUS_ALLOW_REMOTE_MIGRATION_TESTS !== "true") {
    throw new Error(
      "Refusing to create a scratch migration-test database on a non-local host. "
        + "Set BUCEPHALUS_MIGRATION_TEST_DATABASE_URL to a local/CI Postgres URL, "
        + "or set BUCEPHALUS_ALLOW_REMOTE_MIGRATION_TESTS=true for an intentional staging rehearsal.",
    );
  }
}

function databaseUrlFor(databaseUrl: string, databaseName: string): string {
  const parsed = new URL(databaseUrl);
  parsed.pathname = `/${databaseName}`;
  return parsed.toString();
}

function adminDatabaseUrlFor(databaseUrl: string): string {
  return databaseUrlFor(databaseUrl, "postgres");
}

function quoteIdentifier(value: string): string {
  if (!/^[a-z_][a-z0-9_]*$/.test(value)) {
    throw new Error(`Unsafe test database identifier: ${value}`);
  }
  return `"${value}"`;
}

async function createScratchDatabase(baseUrl: string): Promise<{ databaseName: string; databaseUrl: string; adminSql: Sql }> {
  requireSafeDatabaseServer(baseUrl);
  const adminSql = createSql(adminDatabaseUrlFor(baseUrl));
  const databaseName = `bucephalus_migration_test_${Date.now()}_${Math.random().toString(16).slice(2)}`;
  const quotedDatabaseName = quoteIdentifier(databaseName);
  try {
    await adminSql.unsafe(`create database ${quotedDatabaseName}`);
    return {
      databaseName,
      databaseUrl: databaseUrlFor(baseUrl, databaseName),
      adminSql,
    };
  } catch (error) {
    await adminSql.end();
    throw error;
  }
}

async function dropScratchDatabase(adminSql: Sql, databaseName: string): Promise<void> {
  await adminSql.unsafe(`drop database if exists ${quoteIdentifier(databaseName)} with (force)`);
}

async function expectRegclass(sql: Sql, name: string): Promise<void> {
  const [row] = await sql<{ regclass: string | null }[]>`
    select to_regclass(${name})::text as regclass
  `;
  expect(row?.regclass).toBe(name);
}

describe("cloud SQL migrations", () => {
  test("apply from empty Postgres and are idempotent", async () => {
    const baseUrl = migrationTestBaseUrl();
    const scratch = await createScratchDatabase(baseUrl);

    try {
      const expectedMigrations = await migrationFiles();
      await runMigrations({ databaseUrl: scratch.databaseUrl, runtimeRoleName: null });

      const sql = createSql(scratch.databaseUrl);
      try {
        const migrationRows = await sql<{ migration_name: string }[]>`
          select migration_name
          from cloud_schema_migrations
          order by migration_name
        `;
        expect(migrationRows.map((row) => row.migration_name)).toEqual(expectedMigrations);

        const extensionRows = await sql<{ extname: string }[]>`
          select extname from pg_extension where extname = 'pgcrypto'
        `;
        expect(extensionRows).toHaveLength(1);

        const requiredSchemas = ["registry", "fact", "ingest", "cloud", "bucephalus_runtime"];
        const schemaRows = await sql<{ schema_name: string }[]>`
          select schema_name
          from information_schema.schemata
          where schema_name in ${sql(requiredSchemas)}
          order by schema_name
        `;
        expect(schemaRows.map((row) => row.schema_name)).toEqual([...requiredSchemas].sort());

        for (const tableName of [
          "registry.content_objects",
          "ingest.uploads",
          "ingest.import_jobs",
          "cloud.package_artifacts",
          "cloud.runs",
          "cloud.run_attempts",
          "cloud.runner_pools",
          "cloud.runner_worker_images",
          "cloud.runner_instances",
          "cloud.runner_provision_requests",
          "cloud.latch_submissions",
          "bucephalus_runtime.runs",
          "bucephalus_runtime.trial_rows",
          "bucephalus_runtime.trial_conclusion_rows",
        ]) {
          await expectRegclass(sql, tableName);
        }

        const contentKindRows = await sql<{ enumlabel: string }[]>`
          select e.enumlabel
          from pg_type t
          join pg_namespace n on n.oid = t.typnamespace
          join pg_enum e on e.enumtypid = t.oid
          where n.nspname = 'registry'
            and t.typname = 'content_kind'
          order by e.enumsortorder
        `;
        expect(contentKindRows.map((row) => row.enumlabel)).toContain("benchmark");

        const packageDigest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        await sql`
          insert into cloud.package_artifacts (
            package_digest,
            manifest_json,
            resolved_experiment_json
          )
          values (
            ${packageDigest},
            ${sql.json({ schema_version: "manifest_v1" })},
            ${sql.json({ schema_version: "resolved_experiment_v1" })}
          )
        `;
        await sql`
          insert into cloud.runs (package_digest, run_label)
          values (${packageDigest}, 'migration-gate')
        `;

        const runRepository = new RunRepository(sql);
        const runnerRepository = new RunnerRepository(sql);
        const claimCapabilities: WorkerCapabilities = {
          executors: ["runner-docker"],
          resources: ["core_runner", "docker_daemon", "registry_pull", "secret_resolver"],
          arch: "x86_64",
          cpu_count: 4,
          memory_mb: 8192,
          disk_mb: 65536,
          isolation: ["reusable_vm"],
        };
        const claimRequirements: RunRequirements = {
          executor: "runner-docker",
          requires: ["core_runner", "docker_daemon", "registry_pull", "secret_resolver"],
          image_refs: [],
          secret_ids: [],
          network_perimeter: {
            default: "none",
            task_sandbox: "none",
            agent: "none",
            egress_hosts: [],
          },
          sidecars: [],
          accelerators: [],
          arch: "x86_64",
          cpu_count: 2,
          memory_mb: 4096,
          disk_mb: 32768,
          isolation: "reusable_vm",
          timeout_ms: 15 * 60 * 1000,
          max_parallel_trials: 1,
        };
        const unpromotedPool = await runnerRepository.createPool({
          name: "unpromoted-claim-pool",
          capabilities: claimCapabilities,
          metadata: { source: "migration-claim-regression" },
        });
        const unpromotedInstance = await runnerRepository.registerInstance({
          runnerPoolId: unpromotedPool.runner_pool_id,
          instanceName: "unpromoted-runner",
          capabilities: claimCapabilities,
          metadata: { worker_image_ref: "us-central1-docker.pkg.dev/project/repo/worker@sha256:1111111111111111111111111111111111111111111111111111111111111111" },
        });
        await runRepository.createRun({
          packageDigest,
          runLabel: "unpromoted-worker-image-claim",
          env: {},
          secretRefs: {},
          runtimeOptions: {},
          runRequirements: claimRequirements,
          packageProvenance: { schema_version: "cloud_package_provenance_v1", status: "hosted_attested" },
        });
        await expect(runRepository.claimNextRun({
          runnerInstanceId: unpromotedInstance.runner_instance_id,
          leaseSeconds: 30,
        })).rejects.toMatchObject({
          status: 404,
          code: "runner_instance_not_claimable",
          message: "Runner instance is not online in an active pool with the current promoted worker image",
        });

        const stalePool = await runnerRepository.createPool({
          name: "stale-image-claim-pool",
          capabilities: claimCapabilities,
          metadata: { source: "migration-claim-regression" },
        });
        const oldWorkerImage = "us-central1-docker.pkg.dev/project/repo/worker@sha256:2222222222222222222222222222222222222222222222222222222222222222";
        await runnerRepository.promoteWorkerImage({
          poolId: stalePool.runner_pool_id,
          imageRef: oldWorkerImage,
          registryHost: "us-central1-docker.pkg.dev",
          repository: "project/repo/worker",
          digest: "sha256:2222222222222222222222222222222222222222222222222222222222222222",
          metadata: { source: "migration-claim-regression" },
        });
        const staleInstance = await runnerRepository.registerInstance({
          runnerPoolId: stalePool.runner_pool_id,
          instanceName: "stale-runner",
          capabilities: claimCapabilities,
          metadata: { worker_image_ref: oldWorkerImage },
        });
        await runnerRepository.promoteWorkerImage({
          poolId: stalePool.runner_pool_id,
          imageRef: "us-central1-docker.pkg.dev/project/repo/worker@sha256:3333333333333333333333333333333333333333333333333333333333333333",
          registryHost: "us-central1-docker.pkg.dev",
          repository: "project/repo/worker",
          digest: "sha256:3333333333333333333333333333333333333333333333333333333333333333",
          metadata: { source: "migration-claim-regression" },
        });
        await runRepository.createRun({
          packageDigest,
          runLabel: "stale-worker-image-claim",
          env: {},
          secretRefs: {},
          runtimeOptions: {},
          runRequirements: claimRequirements,
          packageProvenance: { schema_version: "cloud_package_provenance_v1", status: "hosted_attested" },
        });
        await expect(runRepository.claimNextRun({
          runnerInstanceId: staleInstance.runner_instance_id,
          leaseSeconds: 30,
        })).rejects.toMatchObject({
          status: 404,
          code: "runner_instance_not_claimable",
          message: "Runner instance is not online in an active pool with the current promoted worker image",
        });

        const packageRepository = new PackageRepository(sql);
        const ownerScopedDigest = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        const [hostedUpload] = await sql<{ upload_id: string }[]>`
          insert into ingest.uploads (filename, media_type, owner_key, status)
          values ('hosted.tgz', 'application/gzip', 'issuer:hosted-user', 'completed')
          returning upload_id
        `;
        const [importedUpload] = await sql<{ upload_id: string }[]>`
          insert into ingest.uploads (filename, media_type, owner_key, status)
          values ('imported.tgz', 'application/gzip', 'issuer:import-user', 'completed')
          returning upload_id
        `;
        await packageRepository.upsertArtifact({
          packageDigest: ownerScopedDigest,
          uploadId: hostedUpload!.upload_id,
          storagePath: "object://hosted",
          byteSize: 1,
          mediaType: "application/gzip",
          manifestJson: { schema_version: "sealed_run_package_v2" },
          resolvedExperimentJson: { experiment: { name: "Owner scoped provenance" } },
          target: null,
          imageRefs: [],
          diagnostics: [],
          packageProvenance: {
            schema_version: "cloud_package_provenance_v1",
            status: "hosted_attested",
            source: "hosted_core",
            message: "hosted owner provenance",
          },
          ownerKey: "issuer:hosted-user",
        });
        await packageRepository.upsertArtifact({
          packageDigest: ownerScopedDigest,
          uploadId: importedUpload!.upload_id,
          storagePath: "object://imported",
          byteSize: 1,
          mediaType: "application/gzip",
          manifestJson: { schema_version: "sealed_run_package_v2" },
          resolvedExperimentJson: { experiment: { name: "Owner scoped provenance" } },
          target: null,
          imageRefs: [],
          diagnostics: [],
          packageProvenance: {
            schema_version: "cloud_package_provenance_v1",
            status: "external_unattested",
            source: "sealed_package_import",
            message: "import owner provenance",
          },
          ownerKey: "issuer:import-user",
        });

        const hostedOwnerArtifact = await packageRepository.getArtifact(ownerScopedDigest, "issuer:hosted-user");
        const importOwnerArtifact = await packageRepository.getArtifact(ownerScopedDigest, "issuer:import-user");
        expect(hostedOwnerArtifact?.package_provenance.status).toBe("hosted_attested");
        expect(hostedOwnerArtifact?.upload_id).toBe(hostedUpload!.upload_id);
        expect(hostedOwnerArtifact?.storage_path).toBe("object://hosted");
        expect(hostedOwnerArtifact?.byte_size).toBe(1);
        expect(hostedOwnerArtifact?.media_type).toBe("application/gzip");
        expect(importOwnerArtifact?.package_provenance.status).toBe("external_unattested");
        expect(importOwnerArtifact?.upload_id).toBe(importedUpload!.upload_id);
        expect(importOwnerArtifact?.storage_path).toBe("object://imported");
        expect(importOwnerArtifact?.byte_size).toBe(1);
        expect(importOwnerArtifact?.media_type).toBe("application/gzip");

        const hostedOwnerList = await packageRepository.listArtifacts({ ownerKey: "issuer:hosted-user" });
        const importOwnerList = await packageRepository.listArtifacts({ ownerKey: "issuer:import-user" });
        expect(hostedOwnerList.find((artifact) => artifact.package_digest === ownerScopedDigest)?.storage_path).toBe("object://hosted");
        expect(hostedOwnerList.find((artifact) => artifact.package_digest === ownerScopedDigest)?.package_provenance.status).toBe("hosted_attested");
        expect(importOwnerList.find((artifact) => artifact.package_digest === ownerScopedDigest)?.storage_path).toBe("object://imported");
        expect(importOwnerList.find((artifact) => artifact.package_digest === ownerScopedDigest)?.package_provenance.status).toBe("external_unattested");

        const oversizedDigest = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
        await sql`
          insert into cloud.package_artifacts (
            package_digest,
            byte_size,
            manifest_json,
            resolved_experiment_json
          )
          values (
            ${oversizedDigest},
            9007199254740993,
            ${sql.json({ schema_version: "sealed_run_package_v2" })},
            ${sql.json({ experiment: { name: "Unsafe byte size" } })}
          )
        `;
        await expect(packageRepository.getArtifact(oversizedDigest)).rejects.toThrow("Persisted package artifact byte_size is invalid");

        await runMigrations({ databaseUrl: scratch.databaseUrl, runtimeRoleName: null });

        const [runCount] = await sql<{ row_count: number }[]>`
          select count(*)::int as row_count
          from cloud.runs
          where package_digest = ${packageDigest}
            and run_label = 'migration-gate'
        `;
        expect(runCount?.row_count).toBe(1);

        const migrationRowsAfterSecondRun = await sql<{ migration_name: string }[]>`
          select migration_name
          from cloud_schema_migrations
          order by migration_name
        `;
        expect(migrationRowsAfterSecondRun.map((row) => row.migration_name)).toEqual(expectedMigrations);
      } finally {
        await sql.end();
      }
    } finally {
      await dropScratchDatabase(scratch.adminSql, scratch.databaseName);
      await scratch.adminSql.end();
    }
  }, 60_000);
});

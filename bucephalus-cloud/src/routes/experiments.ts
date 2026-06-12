import { authOwnerKey, type AuthContext } from "../auth";
import { jsonResponse, optionalString, readJsonObject } from "../http";
import { ImportRepository } from "../imports/repository";
import { importJobToWire, importSealedPackageUpload } from "./imports";
import { PackageRepository, RunRepository } from "../packages/repository";
import { RunnerRepository } from "../runners/repository";
import type { CloudSecretRepository } from "../secrets/repository";
import { diagnoseCloudRunRequest, packageSecretRequirements } from "./runs";

export async function handleExperimentRoute(
  request: Request,
  url: URL,
  imports: ImportRepository,
  packages: PackageRepository,
  _runs: RunRepository,
  runners: RunnerRepository,
  auth?: AuthContext | null,
  secrets?: CloudSecretRepository,
): Promise<Response | null> {
  const ownerKey = authOwnerKey(auth);

  if (request.method === "POST" && url.pathname === "/v1/experiments/builds") {
    const body = await readJsonObject(request);
    const job = await importSealedPackageUpload(body, imports, packages, ownerKey);
    return jsonResponse({
      build_id: job.import_id,
      status: job.status,
      label: optionalString(body.label, "/label"),
      package_digest: job.package_digest,
      import: importJobToWire(job),
    }, { status: 201 });
  }

  if (request.method === "POST" && url.pathname === "/v1/experiments/doctor") {
    const body = await readJsonObject(request);
    const diagnosis = await diagnoseCloudRunRequest({
      body,
      packages,
      runners,
      ownerKey,
      secrets,
      requireHostedSecretRefs: true,
    });
    return jsonResponse({
      ok: true,
      status: "runnable",
      package_digest: diagnosis.artifact.package_digest,
      package_status: diagnosis.artifact.status,
      name: packageName(diagnosis.artifact.resolved_experiment_json),
      image_refs: diagnosis.artifact.image_refs,
      secret_requirements: packageSecretRequirements(diagnosis.artifact),
      supplied_secret_ids: Object.keys(diagnosis.secretRefs).sort(),
      runtime_options: diagnosis.runtimeOptions,
      run_requirements: diagnosis.runRequirements,
    });
  }

  return null;
}

function packageName(resolvedExperiment: Record<string, unknown>): string | null {
  const experiment = resolvedExperiment.experiment;
  if (!isRecord(experiment)) {
    return null;
  }
  const name = experiment.name;
  return typeof name === "string" && name.trim().length > 0 ? name.trim() : null;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

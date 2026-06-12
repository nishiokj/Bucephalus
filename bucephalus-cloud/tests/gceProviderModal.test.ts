import { describe, expect, test } from "bun:test";
// @ts-ignore: provider scripts are shipped as runtime JavaScript deploy assets.
import { renderStartupScript, validateRequest, workerImageForRequest } from "../deploy/provider/gcp/provision-runner-vm.js";

describe("GCE runner provider Modal bridge", () => {
  test("rejects modal provisioning when Modal config is disabled", () => {
    expect(() => validateRequest(
      { run_requirements: { executor: "modal" } },
      { modal: { enabled: false } } as any,
    )).toThrow("Modal backend configuration is disabled");
  });

  test("accepts modal provisioning when Modal config is enabled", () => {
    expect(() => validateRequest(
      { run_requirements: { executor: "modal", requires: ["core_runner", "modal", "registry_pull"] } },
      { modal: { enabled: true } } as any,
    )).not.toThrow();
  });

  test("renders a worker startup env that advertises modal and fetches secrets on the VM", () => {
    const script = renderStartupScript(startupConfig());

    expect(script).toContain("worker_resources=\"${worker_resources},modal\"");
    expect(script).toContain("worker_executors=\"${worker_executors},modal\"");
    expect(script).toContain("BUCEPHALUS_MODAL_LAUNCHER=/usr/local/bin/bucephalus-modal-launcher");
    expect(script).toContain("MODAL_TOKEN_ID=${modal_token_id}");
    expect(script).toContain("printf 'BUCEPHALUS_MODAL_S3_ACCESS_KEY_ID=%s\\n' \"${modal_s3_access_key_id}\"");
    expect(script).toContain("modal_gcp_artifact_registry_service_account_json_b64=\"$(secret_access \"${MODAL_GCP_ARTIFACT_REGISTRY_SERVICE_ACCOUNT_JSON_SECRET}\" \"${MODAL_GCP_ARTIFACT_REGISTRY_SERVICE_ACCOUNT_JSON_SECRET_VERSION}\" | base64 | tr -d '\\n')\"");
    expect(script).toContain("printf 'BUCEPHALUS_MODAL_GCP_ARTIFACT_REGISTRY_SERVICE_ACCOUNT_JSON_B64=%s\\n' \"${modal_gcp_artifact_registry_service_account_json_b64}\"");
    expect(script).toContain("printf 'BUCEPHALUS_MODAL_GCP_SERVICE_ACCOUNT_JSON_B64=%s\\n' \"${modal_gcp_artifact_registry_service_account_json_b64}\"");
    expect(script).toContain("secret_access \"${MODAL_TOKEN_ID_SECRET}\" \"${MODAL_TOKEN_ID_SECRET_VERSION}\"");
    expect(script).not.toContain("actual-modal-token-secret");
  });

  test("renders Modal GCS sync without requiring S3 access key secrets", () => {
    const script = renderStartupScript({
      ...startupConfig(),
      modal: {
        ...startupConfig().modal,
        s3Bucket: "gen-lang-client-0255842044-buc-bucephalus-objects",
        s3EndpointUrl: "https://storage.googleapis.com",
        s3Region: "",
        s3AccessKeyIdSecret: "",
        s3AccessKeyIdSecretVersion: "",
        s3SecretAccessKeySecret: "",
        s3SecretAccessKeySecretVersion: "",
      },
    });

    expect(script).toContain("modal_uses_gcs_service_account_sync=true");
    expect(script).toContain("if [[ -z \"${MODAL_S3_SECRET_NAME}\" && \"${modal_uses_gcs_service_account_sync}\" != \"true\" ]]; then");
    expect(script).toContain("elif [[ \"${modal_uses_gcs_service_account_sync}\" != \"true\" ]]; then");
    expect(script).toContain("printf 'BUCEPHALUS_MODAL_GCP_SERVICE_ACCOUNT_JSON_B64=%s\\n' \"${modal_gcp_artifact_registry_service_account_json_b64}\"");
  });

  test("fails fast for Modal path-style S3 because the Modal Go SDK cannot mount it", () => {
    expect(() => renderStartupScript({
      ...startupConfig(),
      modal: {
        ...startupConfig().modal,
        s3ForcePathStyle: "true",
      },
    })).toThrow("does not support path-style S3 mounts");
  });

  test("resolves worker image from provision request active pool state", () => {
    expect(workerImageForRequest(
      { worker_image: "us-central1-docker.pkg.dev/project/repo/worker@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" },
      { workerImageFallback: "us-central1-docker.pkg.dev/project/repo/worker@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" } as any,
    )).toBe("us-central1-docker.pkg.dev/project/repo/worker@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
  });

  test("passes the exact worker image ref into worker metadata env", () => {
    const script = renderStartupScript(startupConfig());

    expect(script).toContain("BUCEPHALUS_WORKER_IMAGE_REF=${WORKER_IMAGE}");
  });
  
  test("uses configured worker image only as a compatibility fallback", () => {
    expect(workerImageForRequest(
      {},
      { workerImageFallback: "us-central1-docker.pkg.dev/project/repo/worker@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" } as any,
    )).toBe("us-central1-docker.pkg.dev/project/repo/worker@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
  });

  test("fails fast when neither provision request nor fallback carries a worker image", () => {
    expect(() => workerImageForRequest({}, { workerImageFallback: "" } as any))
      .toThrow("provision request requires /worker_image or BUCEPHALUS_GCP_RUNNER_IMAGE");
  });
});

function startupConfig() {
  return {
    projectId: "project-1",
    apiUrl: "https://api.example",
    runnerPoolId: "pool-1",
    provisionRequestId: "provision-1",
    providerId: "gcp://project-1/us-central1-a/runner-1",
    instanceName: "runner-1",
    runnerIsolation: "single_use_vm",
    networkPolicyEnabled: "false",
    workerImage: "us-central1-docker.pkg.dev/project/repo/worker@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    workerTokenSecret: "worker-token",
    workerTokenSecretVersion: "1",
    registryHost: "us-central1-docker.pkg.dev",
    modal: {
      enabled: true,
      appName: "bucephalus-prod",
      environment: "main",
      tokenIdSecret: "modal-token-id",
      tokenIdSecretVersion: "2",
      tokenSecretSecret: "modal-token-secret",
      tokenSecretSecretVersion: "3",
      s3Bucket: "bucephalus-runtime",
      s3Prefix: "modal/runtime",
      s3EndpointUrl: "https://acct.r2.cloudflarestorage.com",
      s3Region: "auto",
      s3SecretName: "",
      s3AccessKeyIdSecret: "modal-s3-access-key-id",
      s3AccessKeyIdSecretVersion: "4",
      s3SecretAccessKeySecret: "modal-s3-secret-access-key",
      s3SecretAccessKeySecretVersion: "5",
      s3ForcePathStyle: "false",
      gcpArtifactRegistrySecretName: "",
      gcpArtifactRegistryServiceAccountJsonSecret: "modal-gcp-ar-sa-json",
      gcpArtifactRegistryServiceAccountJsonSecretVersion: "6",
    },
  };
}

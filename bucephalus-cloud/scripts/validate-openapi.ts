#!/usr/bin/env bun
import { dirname, join, normalize } from "node:path";
import { parse } from "yaml";

const rootDir = new URL("..", import.meta.url).pathname;
const openApiDir = join(rootDir, "api", "openapi");
const files = [
  "common.yaml",
  "auth.yaml",
  "registry.yaml",
  "drafts.yaml",
  "experiments.yaml",
  "imports.yaml",
  "runs.yaml",
  "secrets.yaml",
  "analysis.yaml",
  "observability.yaml",
];

const documents = new Map<string, unknown>();
let failures = 0;

for (const file of files) {
  const absPath = join(openApiDir, file);
  try {
    const document = parse(await Bun.file(absPath).text());
    documents.set(absPath, document);
    validateOpenApiDocument(absPath, document);
    console.log(`validated ${relative(absPath)}`);
  } catch (error) {
    failures += 1;
    console.error(`OpenAPI validation failed for ${relative(absPath)}: ${message(error)}`);
  }
}

for (const [absPath, document] of documents) {
  try {
    validateRefs(absPath, document);
    validateDocumentContracts(absPath, document);
  } catch (error) {
    failures += 1;
    console.error(`OpenAPI contract validation failed: ${message(error)}`);
  }
}

if (failures > 0) {
  process.exit(1);
}

function validateOpenApiDocument(absPath: string, document: unknown): void {
  assertRecord(document, "root document");
  if (!("openapi" in document) && basename(absPath) !== "common.yaml") {
    throw new Error("missing openapi version");
  }
  if ("openapi" in document && typeof document.openapi !== "string") {
    throw new Error("openapi version must be a string");
  }
  if ("paths" in document && !isRecord(document.paths)) {
    throw new Error("paths must be an object");
  }
  if ("components" in document && !isRecord(document.components)) {
    throw new Error("components must be an object");
  }
}

function validateDocumentContracts(absPath: string, document: unknown): void {
  if (basename(absPath) !== "runs.yaml") {
    return;
  }
  assertRecord(document, "runs.yaml root document");
  const plainRunParams = operationParameterNames(document, "/v1/runs/{run_id}", "get");
  const resourceParams = operationParameterNames(document, "/v1/runs/{run_id}/runtime/resources", "get");
  for (const selectorParam of ["kind", "label_selector", "field_selector"]) {
    if (plainRunParams.has(selectorParam)) {
      throw new Error(`runs.yaml getRun must not declare runtime resource selector parameter ${selectorParam}`);
    }
    if (!resourceParams.has(selectorParam)) {
      throw new Error(`runs.yaml listRunRuntimeResources must declare selector parameter ${selectorParam}`);
    }
  }
  if (resourceParams.has("selector")) {
    throw new Error("runs.yaml listRunRuntimeResources must not declare removed selector alias; use label_selector");
  }
  for (const paginationParam of ["limit", "continue"]) {
    if (!resourceParams.has(paginationParam)) {
      throw new Error(`runs.yaml listRunRuntimeResources must declare pagination parameter ${paginationParam}`);
    }
  }
  validateRunsRuntimeContracts(document);
}

function validateRunsRuntimeContracts(document: Record<string, unknown>): void {
  assertCloudRunIdContract(document);
  assertCreateRunRuntimeOptionsContract(document);

  for (const [path, method, operationId] of [
    ["/v1/runs/{run_id}/runtime/events", "get", "listRunRuntimeEvents"],
    ["/v1/runs/{run_id}/runtime/api-resources", "get", "listRunRuntimeApiResources"],
    ["/v1/runs/{run_id}/runtime/api-resources/{kind}", "get", "getRunRuntimeApiResource"],
    ["/v1/runs/{run_id}/runtime/inspect", "get", "inspectRunRuntime"],
    ["/v1/runs/{run_id}/runtime/resources", "get", "listRunRuntimeResources"],
    ["/v1/runs/{run_id}/runtime/resources/health", "get", "getRunRuntimeResourceHealth"],
    ["/v1/runs/{run_id}/runtime/resources/metrics", "get", "listRunRuntimeResourceMetrics"],
    ["/v1/runs/{run_id}/runtime/resources/watch", "get", "watchRunRuntimeResources"],
    ["/v1/runs/{run_id}/runtime/resources/{kind}/{name}", "get", "describeRunRuntimeResource"],
    ["/v1/runs/{run_id}/runtime/resources/{kind}/{name}", "delete", "deleteRunRuntimeResource"],
    ["/v1/runs/{run_id}/runtime/resources/{kind}/{name}/status", "get", "getRunRuntimeResourceStatus"],
    ["/v1/runs/{run_id}/runtime/resources/{kind}/{name}/operations/{operation}", "get", "reviewRunRuntimeResourceOperation"],
    ["/v1/runs/{run_id}/runtime/resources/{kind}/{name}/metrics", "get", "getRunRuntimeResourceMetrics"],
    ["/v1/runs/{run_id}/runtime/resources/{kind}/{name}/events", "get", "listRunRuntimeResourceEvents"],
    ["/v1/runs/{run_id}/runtime/resources/{kind}/{name}/logs", "get", "getRunRuntimeResourceLogs"],
    ["/v1/runs/{run_id}/runtime/resources/{kind}/{name}/content", "get", "getRunRuntimeResourceContent"],
    ["/v1/runs/{run_id}/runtime/resources/{kind}/{name}/actions/cordon", "post", "cordonRunRuntimeRunnerInstance"],
    ["/v1/runs/{run_id}/runtime/resources/{kind}/{name}/actions/drain", "post", "drainRunRuntimeRunnerInstance"],
    ["/v1/runs/{run_id}/runtime/resources/{kind}/{name}/actions/uncordon", "post", "uncordonRunRuntimeRunnerInstance"],
    ["/v1/runs/{run_id}/runtime/resources/{kind}/{name}/actions/cancel", "post", "cancelRunRuntimeAccessResource"],
    ["/v1/runs/{run_id}/runtime/resources/{kind}/{name}/actions/complete", "post", "completeRunRuntimePortForward"],
    ["/v1/runs/{run_id}/runtime/resources/{kind}/{name}/port-forward", "post", "createRunRuntimeResourcePortForward"],
    ["/v1/runs/{run_id}/runtime/resources/{kind}/{name}/exec", "post", "createRunRuntimeResourceExec"],
    ["/v1/worker/run-attempts/{attempt_id}/runtime/resources/PortForward", "get", "listWorkerRunAttemptPortForwardResources"],
    ["/v1/worker/run-attempts/{attempt_id}/runtime/resources/PortForward/{access_request_id}/{action}", "post", "updateWorkerRunAttemptPortForwardResource"],
    ["/v1/worker/run-attempts/{attempt_id}/runtime/resources/Exec", "get", "listWorkerRunAttemptExecResources"],
    ["/v1/worker/run-attempts/{attempt_id}/runtime/resources/Exec/{access_request_id}/{action}", "post", "updateWorkerRunAttemptExecResource"],
    ["/v1/worker/runs/expire-leases", "post", "expireRunLeases"],
  ] as const) {
    assertOperationId(document, path, method, operationId);
  }
  assertRuntimeMutationRequestPreconditionContracts(document);
  assertRuntimeAccessCreateResponseContracts(document);
  assertWorkerRuntimeResourceResponseContracts(document);
  assertWorkerExpireLeasesResponseContract(document);

  for (const [path, expectedRef] of [
    ["/v1/runs/{run_id}/runtime/api-resources", "#/components/schemas/RuntimeApiResourceList"],
    ["/v1/runs/{run_id}/runtime/api-resources/{kind}", "#/components/schemas/RuntimeApiResource"],
    ["/v1/runs/{run_id}/runtime/inspect", "#/components/schemas/RuntimeInspectBundle"],
    ["/v1/runs/{run_id}/runtime/resources", "#/components/schemas/RuntimeResourceList"],
    ["/v1/runs/{run_id}/runtime/resources/health", "#/components/schemas/RuntimeResourceHealth"],
    ["/v1/runs/{run_id}/runtime/resources/metrics", "#/components/schemas/RuntimeResourceMetricsList"],
    ["/v1/runs/{run_id}/runtime/resources/watch", "#/components/schemas/RuntimeResourceWatchList"],
    ["/v1/runs/{run_id}/runtime/resources/{kind}/{name}/status", "#/components/schemas/RuntimeResourceStatus"],
    ["/v1/runs/{run_id}/runtime/resources/{kind}/{name}/operations/{operation}", "#/components/schemas/RuntimeResourceOperationReview"],
    ["/v1/runs/{run_id}/runtime/resources/{kind}/{name}/metrics", "#/components/schemas/RuntimeResourceMetrics"],
    ["/v1/runs/{run_id}/runtime/resources/{kind}/{name}/events", "#/components/schemas/RuntimeResourceEventList"],
  ] as const) {
    assertOperationResponseHasRef(document, path, "get", "200", expectedRef);
  }
  assertOperationResponseHasRef(
    document,
    "/v1/runs/{run_id}/runtime/resources/{kind}/{name}",
    "get",
    "200",
    "#/components/schemas/RuntimeResource",
  );
  assertOperationResponseHasRef(
    document,
    "/v1/runs/{run_id}/runtime/resources/{kind}/{name}",
    "get",
    "200",
    "#/components/schemas/RuntimeResourceDescribe",
  );
  assertOperationResponseHasExactJsonRef(
    document,
    "/v1/runs/{run_id}/runtime/resources/{kind}/{name}",
    "delete",
    "200",
    "#/components/schemas/RuntimeResourceDescribe",
  );
  for (const action of ["cordon", "drain", "uncordon", "cancel", "complete"]) {
    assertOperationResponseHasExactJsonRef(
      document,
      `/v1/runs/{run_id}/runtime/resources/{kind}/{name}/actions/${action}`,
      "post",
      "200",
      "#/components/schemas/RuntimeResourceDescribe",
    );
  }
  assertOperationRequestHasRef(
    document,
    "/v1/runs/{run_id}/runtime/resources/{kind}/{name}/port-forward",
    "post",
    "#/components/schemas/CreateRuntimeResourcePortForwardRequest",
  );
  assertOperationResponseHasRef(
    document,
    "/v1/runs/{run_id}/runtime/resources/{kind}/{name}/port-forward",
    "post",
    "201",
    "#/components/schemas/RuntimeResource",
  );
  assertOperationRequestHasRef(
    document,
    "/v1/runs/{run_id}/runtime/resources/{kind}/{name}/exec",
    "post",
    "#/components/schemas/CreateRuntimeResourceExecRequest",
  );
  assertOperationResponseHasRef(
    document,
    "/v1/runs/{run_id}/runtime/resources/{kind}/{name}/exec",
    "post",
    "201",
    "#/components/schemas/RuntimeResource",
  );
  for (const path of [
    "/v1/runs/{run_id}/runtime",
    "/v1/runs/{run_id}/runtime/port-forwards",
    "/v1/runs/{run_id}/runtime/port-forwards/{access_request_id}",
    "/v1/runs/{run_id}/runtime/execs",
    "/v1/runs/{run_id}/runtime/execs/{access_request_id}",
    "/v1/runs/{run_id}/runtime/results",
    "/v1/runs/{run_id}/runtime/artifacts/{trial_id}/{role}",
    "/v1/runs/{run_id}/runtime/kv/{key}",
  ]) {
    if (pathObject(document, path)) {
      throw new Error(`runs.yaml must not document removed runtime compatibility path ${path}`);
    }
  }
  const serializedRunsOpenapi = JSON.stringify(document);
  for (const forbidden of [
    "RuntimeCompatibilityDeprecation",
    "RuntimeCompatibilityWarning",
    "RuntimeCompatibilityMode",
    "RuntimeCompatibilitySuccessorPath",
    "Deprecated compatibility endpoint",
    "RuntimeSummary",
    "RuntimeResults",
    "RuntimeTrialResult",
    "RuntimeMetricObservation",
    "RuntimeContractStage",
    "RuntimeAttemptObject",
  ]) {
    if (serializedRunsOpenapi.includes(forbidden)) {
      throw new Error(`runs.yaml must not preserve removed runtime compatibility OpenAPI surface ${forbidden}`);
    }
  }

  for (const parameter of ["kind", "label_selector", "field_selector"]) {
    assertOperationParameter(document, "/v1/runs/{run_id}/runtime/resources/health", "get", parameter);
    assertOperationParameter(document, "/v1/runs/{run_id}/runtime/resources/metrics", "get", parameter);
    assertOperationParameter(document, "/v1/runs/{run_id}/runtime/resources/watch", "get", parameter);
  }
  assertOperationParameter(document, "/v1/runs/{run_id}/runtime/resources/metrics", "get", "limit");
  assertOperationParameter(document, "/v1/runs/{run_id}/runtime/resources/metrics", "get", "continue");
  assertOperationParameter(document, "/v1/runs/{run_id}/runtime/api-resources/{kind}", "get", "kind");
  for (const path of [
    "/v1/runs/{run_id}/runtime/resources/health",
    "/v1/runs/{run_id}/runtime/resources/metrics",
    "/v1/runs/{run_id}/runtime/resources/watch",
  ]) {
    if (operationParameterNames(document, path, "get").has("selector")) {
      throw new Error(`runs.yaml ${path} must not declare removed selector alias; use label_selector`);
    }
  }
  assertOperationParameter(document, "/v1/runs/{run_id}/runtime/resources/watch", "get", "known_resource");
  assertOperationParameter(document, "/v1/runs/{run_id}/runtime/resources/watch", "get", "allow_bookmarks");
  assertOperationParameterDescriptionIncludes(document, "/v1/runs/{run_id}/runtime/resources/watch", "get", "allow_bookmarks", "BOOKMARK");
  for (const parameter of ["limit", "after_row_seq", "event_type", "source", "resource_kind", "resource_name", "trial_id", "task_id"]) {
    assertOperationParameter(document, "/v1/runs/{run_id}/runtime/events", "get", parameter);
    assertOperationParameter(document, "/v1/runs/{run_id}/runtime/resources/{kind}/{name}/events", "get", parameter);
  }
  for (const path of [
    "/v1/runs/{run_id}/runtime/events",
    "/v1/runs/{run_id}/runtime/resources/{kind}/{name}/events",
  ]) {
    assertOperationParameter(document, path, "get", "continue");
    assertOperationResponseHasRef(document, path, "get", "400", "./common.yaml#/components/responses/BadRequest");
    assertOperationParameterDescriptionIncludes(document, path, "get", "after_row_seq", "rejects requests that provide both");
    assertOperationParameterDescriptionIncludes(document, path, "get", "continue", "metadata.resourceVersion");
    assertOperationParameterDescriptionIncludes(document, path, "get", "continue", "event-row-seq:<row_seq>");
    assertOperationParameterDescriptionIncludes(document, path, "get", "event_type", "runtime.api_resources.read");
    assertOperationParameterDescriptionIncludes(document, path, "get", "event_type", "runtime.api_resources.read.failed");
    assertOperationParameterDescriptionIncludes(document, path, "get", "event_type", "runtime.inspect.bundle.read");
    assertOperationParameterDescriptionIncludes(document, path, "get", "event_type", "runtime.inspect.bundle.read.failed");
    assertOperationParameterDescriptionIncludes(document, path, "get", "event_type", "raw log/content reads");
  }
  for (const parameter of ["stream", "tail_lines"]) {
    assertOperationParameter(document, "/v1/runs/{run_id}/runtime/resources/{kind}/{name}/logs", "get", parameter);
  }
  assertOperationParameter(document, "/v1/runs/{run_id}/runtime/resources/{kind}/{name}/operations/{operation}", "get", "operation");
  const operationReview = operationObject(document, "/v1/runs/{run_id}/runtime/resources/{kind}/{name}/operations/{operation}", "get");
  const operationReviewDescription = typeof operationReview.description === "string" ? operationReview.description : "";
  if (!operationReviewDescription.includes("logs%2Fstdout")) {
    throw new Error("runs.yaml runtime operation review must document percent-encoded slash-qualified operation labels");
  }
  if (!operationReviewDescription.includes("status-subresource wait checks")) {
    throw new Error("runs.yaml runtime operation review must document wait as a status-subresource operation label");
  }
  const deleteRuntimeResource = operationObject(document, "/v1/runs/{run_id}/runtime/resources/{kind}/{name}", "delete");
  const deleteKindParameter = Array.isArray(deleteRuntimeResource.parameters)
    ? deleteRuntimeResource.parameters.filter(isRecord).find((parameter) => parameter.name === "kind")
    : null;
  assertRecord(deleteKindParameter, "paths./v1/runs/{run_id}/runtime/resources/{kind}/{name}.delete.parameters.kind");
  const deleteKindSchema = deleteKindParameter.schema;
  assertRecord(deleteKindSchema, "paths./v1/runs/{run_id}/runtime/resources/{kind}/{name}.delete.parameters.kind.schema");
  if (Array.isArray(deleteKindSchema.enum)) {
    throw new Error("runs.yaml runtime DELETE kind parameter must accept runtime API aliases; do not constrain it to canonical PortForward/Exec enums");
  }
  for (const header of [
    "X-Bucephalus-Run-Id",
    "X-Bucephalus-Resource-Kind",
    "X-Bucephalus-Resource-Name",
    "X-Bucephalus-Core-Run-Id",
    "X-Bucephalus-Trial-Id",
    "X-Bucephalus-Artifact-Role",
    "X-Bucephalus-Object-Ref",
    "X-Bucephalus-Artifact-Sha256",
  ]) {
    assertOperationResponseHeader(document, "/v1/runs/{run_id}/runtime/resources/{kind}/{name}/content", "get", "200", header);
  }
  for (const header of [
    "X-Bucephalus-Run-Id",
    "X-Bucephalus-Resource-Kind",
    "X-Bucephalus-Resource-Name",
    "X-Bucephalus-Log-Stream",
    "X-Bucephalus-Core-Run-Id",
    "X-Bucephalus-Trial-Id",
    "X-Bucephalus-Artifact-Role",
    "X-Bucephalus-Object-Ref",
    "X-Bucephalus-Artifact-Sha256",
  ]) {
    assertOperationResponseHeader(document, "/v1/runs/{run_id}/runtime/resources/{kind}/{name}/logs", "get", "200", header);
  }

  assertSchemaRequired(document, "RuntimeResource", ["apiVersion", "kind", "metadata", "spec", "status", "audit"]);
  assertSchemaRequired(document, "RuntimeResourceList", ["apiVersion", "kind", "metadata", "cloud_run_id", "core_run_ids", "resources"]);
  assertSchemaRequired(document, "RuntimeResourceListMeta", ["resourceVersion", "continue", "remainingItemCount", "total", "returned"]);
  assertSchemaRequired(document, "RuntimeEventListMetadata", ["resourceVersion", "continue", "remainingItemCount", "limit", "returned", "after_row_seq", "next_after_row_seq"]);
  assertSchemaRequired(document, "RuntimeEvent", ["source", "core_run_id", "trial_id", "schedule_idx", "attempt", "row_seq", "slot_commit_id", "variant_id", "task_id", "repl_idx", "seq", "event_type", "ts", "resource_refs", "payload", "row"]);
  assertSchemaRequired(document, "RuntimeResourceEventList", ["apiVersion", "kind", "cloud_run_id", "generated_at", "core_run_ids", "resource", "event_filter", "metadata", "events"]);
  assertSchemaRequired(document, "RuntimeResourceHealth", ["apiVersion", "kind", "cloud_run_id", "generated_at", "core_run_ids", "summary", "resources"]);
  assertSchemaRequired(document, "RuntimeResourceHealthSummary", [
    "total",
    "ready",
    "degraded",
    "problem",
    "unknown",
    "access_targets",
    "reachable_access_targets",
    "port_forward_ready",
    "exec_ready",
    "actions_available",
    "observed_resources",
    "observed_current",
    "observed_stale",
    "observed_unknown",
  ]);
  assertSchemaRequired(document, "RuntimeResourceHealthRow", ["resource", "resource_ref", "health", "phase", "ready", "reason", "message", "condition_summary", "degraded_conditions", "actions", "access", "access_summary", "source", "updated_at", "resource_version"]);
  assertSchemaPropertiesPresent(document, "RuntimeResourceAccessStatus", ["reachable", "reason", "port_forward", "exec", "runner_instance_id", "runner_instance_status", "attempt_id", "worker_id"]);
  const runtimeResourceHealthRow = componentSchema(document, "RuntimeResourceHealthRow");
  const healthRowAccess = schemaProperty(runtimeResourceHealthRow, "access", "RuntimeResourceHealthRow");
  if ((healthRowAccess as Record<string, unknown>).$ref !== "#/components/schemas/RuntimeResourceAccessStatus") {
    throw new Error("RuntimeResourceHealthRow.access must use RuntimeResourceAccessStatus");
  }
  assertSchemaRequired(document, "RuntimeResourceCondition", ["type", "status", "reason", "message"]);
  assertSchemaRequired(document, "RuntimeResourceWatchList", ["apiVersion", "kind", "cloud_run_id", "generated_at", "core_run_ids", "resource_versions", "events", "resource_inventory"]);
  const runtimeResourceWatchList = componentSchema(document, "RuntimeResourceWatchList");
  assertSchemaPropertyRef(runtimeResourceWatchList, "cloud_run_id", "RuntimeResourceWatchList", "#/components/schemas/CloudRunId");
  const watchGeneratedAt = schemaProperty(runtimeResourceWatchList, "generated_at", "RuntimeResourceWatchList");
  if (watchGeneratedAt.type !== "string" || watchGeneratedAt.format !== "date-time") {
    throw new Error("RuntimeResourceWatchList.generated_at must be a date-time string");
  }
  const watchCoreRunIds = schemaProperty(runtimeResourceWatchList, "core_run_ids", "RuntimeResourceWatchList");
  if (watchCoreRunIds.type !== "array") {
    throw new Error("RuntimeResourceWatchList.core_run_ids must be an array");
  }
  assertRecord(watchCoreRunIds.items, "RuntimeResourceWatchList.core_run_ids.items");
  if (watchCoreRunIds.items.type !== "string") {
    throw new Error("RuntimeResourceWatchList.core_run_ids items must be strings");
  }
  const watchEvents = schemaProperty(runtimeResourceWatchList, "events", "RuntimeResourceWatchList");
  if (watchEvents.type !== "array") {
    throw new Error("RuntimeResourceWatchList.events must be an array");
  }
  assertRecord(watchEvents.items, "RuntimeResourceWatchList.events.items");
  if (watchEvents.items.$ref !== "#/components/schemas/RuntimeResourceWatchEvent") {
    throw new Error("RuntimeResourceWatchList.events.items must use RuntimeResourceWatchEvent");
  }
  assertSchemaPropertyRef(runtimeResourceWatchList, "resource_inventory", "RuntimeResourceWatchList", "#/components/schemas/RuntimeResourceList");
  assertSchemaRequired(document, "RuntimeResourceWatchEvent", ["type", "resource_ref"]);
  const runtimeResourceWatchEvent = componentSchema(document, "RuntimeResourceWatchEvent");
  const watchEventType = schemaProperty(runtimeResourceWatchEvent, "type", "RuntimeResourceWatchEvent");
  const watchEventTypeEnum = watchEventType.enum;
  if (!Array.isArray(watchEventTypeEnum) || !["ADDED", "MODIFIED", "DELETED", "BOOKMARK"].every((type) => watchEventTypeEnum.includes(type))) {
    throw new Error("RuntimeResourceWatchEvent.type must enumerate ADDED, MODIFIED, DELETED, and BOOKMARK");
  }
  assertSchemaPropertyRef(runtimeResourceWatchEvent, "resource_ref", "RuntimeResourceWatchEvent", "#/components/schemas/RuntimeResourceWatchRef");
  for (const propertyName of ["resource_version", "previous_resource_version"]) {
    const property = schemaProperty(runtimeResourceWatchEvent, propertyName, "RuntimeResourceWatchEvent");
    if (property.type !== "string") {
      throw new Error(`RuntimeResourceWatchEvent.${propertyName} must be a string cursor`);
    }
  }
  assertSchemaPropertyRef(runtimeResourceWatchEvent, "resource", "RuntimeResourceWatchEvent", "#/components/schemas/RuntimeResource");
  assertSchemaRequired(document, "RuntimeResourceWatchRef", ["apiVersion", "kind", "name"]);
  assertSchemaRequired(document, "RuntimeResourceDescribe", ["apiVersion", "kind", "cloud_run_id", "generated_at", "core_run_ids", "resource", "operations", "related_resources", "event_list"]);
  const runtimeResourceDescribe = componentSchema(document, "RuntimeResourceDescribe");
  assertSchemaPropertyRef(runtimeResourceDescribe, "cloud_run_id", "RuntimeResourceDescribe", "#/components/schemas/CloudRunId");
  const describeGeneratedAt = schemaProperty(runtimeResourceDescribe, "generated_at", "RuntimeResourceDescribe");
  if (describeGeneratedAt.type !== "string" || describeGeneratedAt.format !== "date-time") {
    throw new Error("RuntimeResourceDescribe.generated_at must be a date-time string");
  }
  const describeCoreRunIds = schemaProperty(runtimeResourceDescribe, "core_run_ids", "RuntimeResourceDescribe");
  if (describeCoreRunIds.type !== "array") {
    throw new Error("RuntimeResourceDescribe.core_run_ids must be an array");
  }
  assertRecord(describeCoreRunIds.items, "RuntimeResourceDescribe.core_run_ids.items");
  if (describeCoreRunIds.items.type !== "string") {
    throw new Error("RuntimeResourceDescribe.core_run_ids items must be strings");
  }
  const describeEventList = schemaProperty(runtimeResourceDescribe, "event_list", "RuntimeResourceDescribe");
  if ((describeEventList as Record<string, unknown>).$ref !== "#/components/schemas/RuntimeResourceEventList") {
    throw new Error("RuntimeResourceDescribe.event_list must use RuntimeResourceEventList");
  }
  assertSchemaRequired(document, "RuntimeResourceOperation", ["purpose", "command", "supported", "reason", "message", "verb", "subresource", "action", "requires_running_run"]);
  assertSchemaRequired(document, "RuntimeResourceOperationReview", ["apiVersion", "kind", "cloud_run_id", "generated_at", "core_run_ids", "resource_ref", "resource_version", "resource_generation", "observed_generation", "operation", "matched_operation", "supported", "reason", "message", "command", "verb", "subresource", "action", "requires_running_run"]);
  const runtimeResourceOperationReview = componentSchema(document, "RuntimeResourceOperationReview");
  assertSchemaPropertyRef(runtimeResourceOperationReview, "cloud_run_id", "RuntimeResourceOperationReview", "#/components/schemas/CloudRunId");
  const operationReviewGeneratedAt = schemaProperty(runtimeResourceOperationReview, "generated_at", "RuntimeResourceOperationReview");
  if (operationReviewGeneratedAt.type !== "string" || operationReviewGeneratedAt.format !== "date-time") {
    throw new Error("RuntimeResourceOperationReview.generated_at must be a date-time string");
  }
  const operationReviewCoreRunIds = schemaProperty(runtimeResourceOperationReview, "core_run_ids", "RuntimeResourceOperationReview");
  if (operationReviewCoreRunIds.type !== "array") {
    throw new Error("RuntimeResourceOperationReview.core_run_ids must be an array");
  }
  assertRecord(operationReviewCoreRunIds.items, "RuntimeResourceOperationReview.core_run_ids.items");
  if (operationReviewCoreRunIds.items.type !== "string") {
    throw new Error("RuntimeResourceOperationReview.core_run_ids items must be strings");
  }
  assertSchemaPropertyRef(runtimeResourceOperationReview, "resource_ref", "RuntimeResourceOperationReview", "#/components/schemas/RuntimeResourceWatchRef");
  assertSchemaRequired(document, "RuntimeResourceStatus", ["apiVersion", "kind", "cloud_run_id", "generated_at", "core_run_ids", "resource_ref", "generation", "observedGeneration", "resourceVersion", "phase", "reason", "message", "conditions", "actions", "status", "audit"]);
  assertRuntimeResourceStatusLifecycleFields(document);
  assertSchemaRequired(document, "RuntimeResourceMetrics", ["apiVersion", "kind", "cloud_run_id", "generated_at", "core_run_ids", "resource_ref", "resource_version", "phase", "summary", "metrics"]);
  assertSchemaRequired(document, "RuntimeResourceMetricsSummary", ["metrics_total", "lifecycle_metrics", "condition_metrics", "access_metrics", "event_metrics", "numeric_spec_metrics", "numeric_status_metrics", "events_total"]);
  assertSchemaRequired(document, "RuntimeResourceMetric", ["name", "value", "unit", "source", "description", "labels"]);
  assertSchemaRequired(document, "RuntimeResourceMetricsList", ["apiVersion", "kind", "cloud_run_id", "generated_at", "core_run_ids", "metadata", "summary", "resources"]);
  assertSchemaRequired(document, "RuntimeInspectBundle", ["apiVersion", "kind", "cloud_run_id", "generated_at", "resource_filter", "api_resources", "resource_inventory", "resource_health", "resource_metrics", "event_list", "log_refs"]);
  const runtimeInspectBundle = componentSchema(document, "RuntimeInspectBundle");
  assertSchemaPropertyRef(runtimeInspectBundle, "resource_filter", "RuntimeInspectBundle", "#/components/schemas/RuntimeInspectBundleFilter");
  assertSchemaPropertyRef(runtimeInspectBundle, "api_resources", "RuntimeInspectBundle", "#/components/schemas/RuntimeApiResourceList");
  assertSchemaPropertyRef(runtimeInspectBundle, "resource_inventory", "RuntimeInspectBundle", "#/components/schemas/RuntimeResourceList");
  assertSchemaPropertyRef(runtimeInspectBundle, "resource_health", "RuntimeInspectBundle", "#/components/schemas/RuntimeResourceHealth");
  assertSchemaPropertyRef(runtimeInspectBundle, "resource_metrics", "RuntimeInspectBundle", "#/components/schemas/RuntimeResourceMetricsList");
  assertSchemaPropertyRef(runtimeInspectBundle, "event_list", "RuntimeInspectBundle", "#/components/schemas/RuntimeEventList");
  const inspectLogRefs = schemaProperty(runtimeInspectBundle, "log_refs", "RuntimeInspectBundle");
  if (inspectLogRefs.type !== "array") {
    throw new Error("RuntimeInspectBundle.log_refs must be an array");
  }
  assertRecord(inspectLogRefs.items, "RuntimeInspectBundle.log_refs.items");
  assertSchemaRequiredFields(inspectLogRefs.items, "RuntimeInspectBundle.log_refs.items", ["resource", "streams", "urls"]);
  const inspectLogRefResource = schemaProperty(inspectLogRefs.items, "resource", "RuntimeInspectBundle.log_refs.items");
  assertSchemaRequiredFields(inspectLogRefResource, "RuntimeInspectBundle.log_refs.items.resource", ["apiVersion", "kind", "name"]);
  const inspectLogRefStreams = schemaProperty(inspectLogRefs.items, "streams", "RuntimeInspectBundle.log_refs.items");
  assertRecord(inspectLogRefStreams.items, "RuntimeInspectBundle.log_refs.items.streams.items");
  if (!Array.isArray(inspectLogRefStreams.items.enum) || !inspectLogRefStreams.items.enum.includes("stdout") || !inspectLogRefStreams.items.enum.includes("stderr")) {
    throw new Error("RuntimeInspectBundle.log_refs.items.streams must enumerate stdout and stderr");
  }
  const inspectLogRefUrls = schemaProperty(inspectLogRefs.items, "urls", "RuntimeInspectBundle.log_refs.items");
  assertSchemaRequiredFields(inspectLogRefUrls, "RuntimeInspectBundle.log_refs.items.urls", ["stdout", "stderr"]);
  assertSchemaRequiredFields(
    schemaProperty(componentSchema(document, "RuntimeResourceMetricsList"), "metadata", "RuntimeResourceMetricsList"),
    "RuntimeResourceMetricsList.metadata",
    ["resourceVersion", "continue", "remainingItemCount", "total", "returned"],
  );
  assertSchemaRequired(document, "RuntimeResourceMetricsListSummary", ["resources_total", "resources_returned", "metrics_total", "lifecycle_metrics", "condition_metrics", "access_metrics", "event_metrics", "numeric_spec_metrics", "numeric_status_metrics", "events_total"]);
  assertSchemaRequired(document, "RuntimeApiResourceList", ["apiVersion", "kind", "cloud_run_id", "generated_at", "core_run_ids", "resources"]);
  const runtimeApiResourceList = componentSchema(document, "RuntimeApiResourceList");
  assertSchemaPropertyRef(runtimeApiResourceList, "cloud_run_id", "RuntimeApiResourceList", "#/components/schemas/CloudRunId");
  const apiResourceListGeneratedAt = schemaProperty(runtimeApiResourceList, "generated_at", "RuntimeApiResourceList");
  if (apiResourceListGeneratedAt.type !== "string" || apiResourceListGeneratedAt.format !== "date-time") {
    throw new Error("RuntimeApiResourceList.generated_at must be a date-time string");
  }
  const apiResourceListCoreRunIds = schemaProperty(runtimeApiResourceList, "core_run_ids", "RuntimeApiResourceList");
  if (apiResourceListCoreRunIds.type !== "array") {
    throw new Error("RuntimeApiResourceList.core_run_ids must be an array");
  }
  assertRecord(apiResourceListCoreRunIds.items, "RuntimeApiResourceList.core_run_ids.items");
  if (apiResourceListCoreRunIds.items.type !== "string") {
    throw new Error("RuntimeApiResourceList.core_run_ids items must be strings");
  }
  assertSchemaRequired(document, "RuntimeApiResource", ["cloud_run_id", "generated_at", "core_run_ids", "verbs", "subresources", "actions", "access", "supports", "pathTemplates", "exampleCommands", "printerColumns", "fieldSelectors", "labelSelectors", "labelSelector"]);
  const runtimeApiResource = componentSchema(document, "RuntimeApiResource");
  assertSchemaPropertyRef(runtimeApiResource, "cloud_run_id", "RuntimeApiResource", "#/components/schemas/CloudRunId");
  const apiResourceGeneratedAt = schemaProperty(runtimeApiResource, "generated_at", "RuntimeApiResource");
  if (apiResourceGeneratedAt.type !== "string" || apiResourceGeneratedAt.format !== "date-time") {
    throw new Error("RuntimeApiResource.generated_at must be a date-time string");
  }
  const apiResourceCoreRunIds = schemaProperty(runtimeApiResource, "core_run_ids", "RuntimeApiResource");
  if (apiResourceCoreRunIds.type !== "array") {
    throw new Error("RuntimeApiResource.core_run_ids must be an array");
  }
  assertRecord(apiResourceCoreRunIds.items, "RuntimeApiResource.core_run_ids.items");
  if (apiResourceCoreRunIds.items.type !== "string") {
    throw new Error("RuntimeApiResource.core_run_ids items must be strings");
  }
  assertSchemaRequired(document, "RuntimeApiResourcePathTemplates", ["collection", "resource", "describe", "operationReview", "watch", "subresources"]);
  assertSchemaPropertiesPresent(document, "RuntimeApiResourcePathTemplates", ["create", "delete"]);
  assertSchemaRequired(document, "RuntimeApiResourceExampleCommand", ["purpose", "command"]);
  assertSchemaRequired(document, "RuntimeApiResourcePrinterColumn", ["name", "type", "jsonPath", "description", "priority"]);
  assertSchemaPropertyDescriptionIncludes(runtimeApiResource, "fieldSelectors", "RuntimeApiResource", "Concrete field selector paths");
  assertSchemaPropertyDescriptionIncludes(runtimeApiResource, "fieldSelectors", "RuntimeApiResource", "list/watch/inspect endpoints");
  assertSchemaPropertyDescriptionIncludes(runtimeApiResource, "fieldSelectors", "RuntimeApiResource", "spec.involved_object.kind");
  assertSchemaPropertyDescriptionIncludes(runtimeApiResource, "fieldSelectors", "RuntimeApiResource", "status.involved_uid");
  const runtimeEventListMetadata = componentSchema(document, "RuntimeEventListMetadata");
  assertSchemaPropertyDescriptionIncludes(runtimeEventListMetadata, "resourceVersion", "RuntimeEventListMetadata", "event-row-seq:<row_seq>");
  assertSchemaPropertyDescriptionIncludes(runtimeEventListMetadata, "resourceVersion", "RuntimeEventListMetadata", "continue");
  assertSchemaPropertyDescriptionIncludes(runtimeEventListMetadata, "continue", "RuntimeEventListMetadata", "event-row-seq:<row_seq>");
  assertSchemaPropertyDescriptionIncludes(runtimeEventListMetadata, "after_row_seq", "RuntimeEventListMetadata", "normalizing either");
  assertSchemaPropertyDescriptionIncludes(runtimeEventListMetadata, "next_after_row_seq", "RuntimeEventListMetadata", "metadata.resourceVersion");
  const runtimeEvent = componentSchema(document, "RuntimeEvent");
  assertSchemaPropertyDescriptionIncludes(runtimeEvent, "resource_refs", "RuntimeEvent", "primary involved runtime object");
  assertSchemaPropertyDescriptionIncludes(runtimeEvent, "payload", "RuntimeEvent", "runtime.api_resources.read");
  assertSchemaPropertyDescriptionIncludes(runtimeEvent, "payload", "RuntimeEvent", "runtime.api_resources.read.failed");
  assertSchemaPropertyDescriptionIncludes(runtimeEvent, "payload", "RuntimeEvent", "runtime.inspect.bundle.read");
  assertSchemaPropertyDescriptionIncludes(runtimeEvent, "payload", "RuntimeEvent", "runtime.inspect.bundle.read.failed");
  assertSchemaPropertyDescriptionIncludes(runtimeEvent, "payload", "RuntimeEvent", "Run/<run_id>");
  assertSchemaPropertyDescriptionIncludes(runtimeEvent, "payload", "RuntimeEvent", "inventory and event");
  assertSchemaPropertyDescriptionIncludes(runtimeEvent, "payload", "RuntimeEvent", "log-reference count");

  const runtimeResource = componentSchema(document, "RuntimeResource");
  const metadata = schemaProperty(runtimeResource, "metadata", "RuntimeResource");
  assertSchemaRequiredFields(metadata, "RuntimeResource.metadata", ["name", "uid", "generation", "resourceVersion", "labels", "annotations", "ownerReferences"]);
  const metadataUid = schemaProperty(metadata, "uid", "RuntimeResource.metadata");
  if (metadataUid.type !== "string" || metadataUid.minLength !== 1) {
    throw new Error("RuntimeResource.metadata.uid must be a non-empty string");
  }
  const metadataGeneration = schemaProperty(metadata, "generation", "RuntimeResource.metadata");
  if (metadataGeneration.type !== "integer" || metadataGeneration.minimum !== 1) {
    throw new Error("RuntimeResource.metadata.generation must be an integer with minimum 1");
  }
  const metadataResourceVersion = schemaProperty(metadata, "resourceVersion", "RuntimeResource.metadata");
  if (metadataResourceVersion.type !== "string" || metadataResourceVersion.minLength !== 1) {
    throw new Error("RuntimeResource.metadata.resourceVersion must be a non-empty string");
  }
  assertSchemaPropertyDescriptionIncludes(metadata, "generation", "RuntimeResource.metadata", "desired-state generation");
  assertSchemaPropertyDescriptionIncludes(metadata, "resourceVersion", "RuntimeResource.metadata", "Opaque Kubernetes-style change token");
  assertSchemaPropertyDescriptionIncludes(metadata, "creationTimestamp", "RuntimeResource.metadata", "Kubernetes-style creation timestamp");
  assertSchemaPropertyDescriptionIncludes(metadata, "deletionTimestamp", "RuntimeResource.metadata", "audited delete/cancel");
  const ownerReferences = schemaProperty(metadata, "ownerReferences", "RuntimeResource.metadata");
  assertRecord(ownerReferences.items, "RuntimeResource.metadata.ownerReferences.items");
  assertSchemaRequiredFields(ownerReferences.items, "RuntimeResource.metadata.ownerReferences.items", ["apiVersion", "kind", "name"]);
  const status = schemaProperty(runtimeResource, "status", "RuntimeResource");
  const statusDescription = typeof status.description === "string" ? status.description : "";
  for (const requiredText of [
    "actions[]",
    "status.access",
    "reachable",
    "port_forward",
    "exec",
    "runner_instance_id",
    "attempt_id",
    "worker_id",
    "status.involved",
    "status.involved_kind",
    "status.involved_name",
    "status.involved_count",
  ]) {
    if (!statusDescription.includes(requiredText)) {
      throw new Error(`RuntimeResource.status description must document ${requiredText}`);
    }
  }

  assertSchemaAbsent(document, "CreateRuntimePortForwardRequest");
  assertSchemaAbsent(document, "CreateRuntimeExecRequest");
  assertSchemaRequired(document, "CreateRuntimeResourcePortForwardRequest", ["target_port", "resource_version"]);
  assertSchemaRequired(document, "CreateRuntimeResourceExecRequest", ["command", "resource_version"]);
  assertSchemaPropertiesAbsent(document, "CreateRuntimeResourcePortForwardRequest", ["resource_kind", "resource_name"]);
  assertSchemaPropertiesAbsent(document, "CreateRuntimeResourceExecRequest", ["resource_kind", "resource_name"]);
  assertSchemaAbsent(document, "RuntimePortForward");
  assertSchemaAbsent(document, "RuntimeExec");
  assertSchemaPropertiesAbsent(document, "CloudRuntimeOptions", ["region", "runtime_region", "placement", "zone", "executor", "cpu"]);
  const cloudRuntimeOptions = componentSchema(document, "CloudRuntimeOptions");
  const cloudRuntimeOptionsDescription = typeof cloudRuntimeOptions.description === "string" ? cloudRuntimeOptions.description : "";
  for (const requiredText of [
    "does not include region",
    "runtime_region",
    "placement",
    "zone",
    "executor",
    "cpu",
  ]) {
    if (!cloudRuntimeOptionsDescription.includes(requiredText)) {
      throw new Error(`CloudRuntimeOptions description must document unsupported hosted Cloud option ${requiredText}`);
    }
  }
}

function assertCreateRunRuntimeOptionsContract(document: Record<string, unknown>): void {
  const { schema, label } = operationRequestJsonSchema(document, "/v1/runs", "post");
  const runtimeOptions = schemaProperty(schema, "runtime_options", label);
  const description = typeof runtimeOptions.description === "string" ? runtimeOptions.description : "";
  for (const requiredText of [
    "must not send region",
    "runtime_region",
    "placement",
    "zone",
    "executor",
    "cpu",
    "backend",
    "cpu_count",
  ]) {
    if (!description.includes(requiredText)) {
      throw new Error(`createRun runtime_options description must document hosted Cloud option contract: ${requiredText}`);
    }
  }
}

function assertRuntimeResourceStatusLifecycleFields(document: Record<string, unknown>): void {
  const status = componentSchema(document, "RuntimeResourceStatus");
  assertSchemaPropertyRef(status, "cloud_run_id", "RuntimeResourceStatus", "#/components/schemas/CloudRunId");
  const generatedAt = schemaProperty(status, "generated_at", "RuntimeResourceStatus");
  if (generatedAt.type !== "string" || generatedAt.format !== "date-time") {
    throw new Error("RuntimeResourceStatus.generated_at must be a date-time string");
  }
  const coreRunIds = schemaProperty(status, "core_run_ids", "RuntimeResourceStatus");
  if (coreRunIds.type !== "array") {
    throw new Error("RuntimeResourceStatus.core_run_ids must be an array");
  }
  assertRecord(coreRunIds.items, "RuntimeResourceStatus.core_run_ids.items");
  if (coreRunIds.items.type !== "string") {
    throw new Error("RuntimeResourceStatus.core_run_ids items must be strings");
  }
  assertSchemaPropertyRef(status, "resource_ref", "RuntimeResourceStatus", "#/components/schemas/RuntimeResourceWatchRef");
  for (const propertyName of ["generation", "observedGeneration"]) {
    const property = schemaProperty(status, propertyName, "RuntimeResourceStatus");
    if (property.type !== "integer" || property.minimum !== 1) {
      throw new Error(`RuntimeResourceStatus.${propertyName} must be a non-null integer with minimum 1`);
    }
  }
  const resourceVersion = schemaProperty(status, "resourceVersion", "RuntimeResourceStatus");
  if (resourceVersion.type !== "string" || resourceVersion.minLength !== 1) {
    throw new Error("RuntimeResourceStatus.resourceVersion must be a non-empty string");
  }
  const deletionTimestamp = schemaProperty(status, "deletionTimestamp", "RuntimeResourceStatus");
  if (!Array.isArray(deletionTimestamp.type) || !deletionTimestamp.type.includes("string") || !deletionTimestamp.type.includes("null")) {
    throw new Error("RuntimeResourceStatus.deletionTimestamp must stay nullable for non-deleting resources");
  }
}

function assertRuntimeMutationRequestPreconditionContracts(document: Record<string, unknown>): void {
  for (const [path, method, requiredFields] of [
    ["/v1/runs/{run_id}/runtime/resources/{kind}/{name}", "delete", ["resource_version"]],
    ["/v1/runs/{run_id}/runtime/resources/{kind}/{name}/actions/cordon", "post", ["resource_version"]],
    ["/v1/runs/{run_id}/runtime/resources/{kind}/{name}/actions/drain", "post", ["resource_version"]],
    ["/v1/runs/{run_id}/runtime/resources/{kind}/{name}/actions/uncordon", "post", ["resource_version"]],
    ["/v1/runs/{run_id}/runtime/resources/{kind}/{name}/actions/cancel", "post", ["resource_version"]],
    ["/v1/runs/{run_id}/runtime/resources/{kind}/{name}/actions/complete", "post", ["resource_version"]],
    ["/v1/runs/{run_id}/runtime/resources/{kind}/{name}/port-forward", "post", ["target_port", "resource_version"]],
    ["/v1/runs/{run_id}/runtime/resources/{kind}/{name}/exec", "post", ["command", "resource_version"]],
  ] as const) {
    const { schema, label } = operationRequestJsonSchema(document, path, method);
    assertSchemaRequiredFields(schema, label, requiredFields);
    assertRuntimeResourceVersionPreconditionField(schema, label);
  }
}

function assertRuntimeResourceVersionPreconditionField(schema: Record<string, unknown>, label: string): void {
  const resourceVersion = schemaProperty(schema, "resource_version", label);
  if (resourceVersion.type !== "string") {
    throw new Error(`${label}.resource_version must be a string`);
  }
  if (resourceVersion.minLength !== 1) {
    throw new Error(`${label}.resource_version must require a non-empty value`);
  }
  const description = typeof resourceVersion.description === "string" ? resourceVersion.description : "";
  for (const requiredText of ["operation review", "Precondition Required", "Conflict"]) {
    if (!description.includes(requiredText)) {
      throw new Error(`${label}.resource_version description must include ${requiredText}`);
    }
  }
}

function assertWorkerRuntimeResourceResponseContracts(document: Record<string, unknown>): void {
  assertOperationResponseEnvelopeProperty(
    document,
    "/v1/worker/run-attempts/{attempt_id}/runtime/resources/PortForward",
    "get",
    "200",
    "resources",
    "#/components/schemas/RuntimeResource",
    "array",
  );
  assertOperationResponseEnvelopeProperty(
    document,
    "/v1/worker/run-attempts/{attempt_id}/runtime/resources/Exec",
    "get",
    "200",
    "resources",
    "#/components/schemas/RuntimeResource",
    "array",
  );
  assertOperationResponseEnvelopeProperty(
    document,
    "/v1/worker/run-attempts/{attempt_id}/runtime/resources/PortForward/{access_request_id}/{action}",
    "post",
    "200",
    "resource",
    "#/components/schemas/RuntimeResource",
    "ref",
  );
  assertOperationResponseEnvelopeProperty(
    document,
    "/v1/worker/run-attempts/{attempt_id}/runtime/resources/Exec/{access_request_id}/{action}",
    "post",
    "200",
    "resource",
    "#/components/schemas/RuntimeResource",
    "ref",
  );
}

function assertRuntimeAccessCreateResponseContracts(document: Record<string, unknown>): void {
  assertOperationResponseEnvelopeProperty(
    document,
    "/v1/runs/{run_id}/runtime/resources/{kind}/{name}/port-forward",
    "post",
    "201",
    "resource",
    "#/components/schemas/RuntimeResource",
    "ref",
  );
  assertOperationResponseEnvelopeProperty(
    document,
    "/v1/runs/{run_id}/runtime/resources/{kind}/{name}/exec",
    "post",
    "201",
    "resource",
    "#/components/schemas/RuntimeResource",
    "ref",
  );
}

function assertWorkerExpireLeasesResponseContract(document: Record<string, unknown>): void {
  const label = "paths./v1/worker/runs/expire-leases.post.responses.200.content.application/json.schema";
  const schema = operationResponseJsonSchema(document, "/v1/worker/runs/expire-leases", "post", "200");
  if (schema.type !== "object" || schema.additionalProperties !== false) {
    throw new Error("runs.yaml expireRunLeases response must use a closed object envelope");
  }
  assertRecord(schema.properties, `${label}.properties`);
  if ("expired_access_requests" in schema.properties) {
    throw new Error("runs.yaml expireRunLeases must not expose expired_access_requests; use expired_access_resources");
  }
  const required = schema.required;
  if (!Array.isArray(required) || required.length !== 2 || !required.includes("expired") || !required.includes("expired_access_resources")) {
    throw new Error("runs.yaml expireRunLeases response must require expired and expired_access_resources");
  }
  const accessResources = schema.properties.expired_access_resources;
  assertRecord(accessResources, `${label}.properties.expired_access_resources`);
  if (accessResources.type !== "array") {
    throw new Error("runs.yaml expireRunLeases expired_access_resources must be an array");
  }
  assertRecord(accessResources.items, `${label}.properties.expired_access_resources.items`);
  if (accessResources.items.$ref !== "#/components/schemas/RuntimeResource") {
    throw new Error("runs.yaml expireRunLeases expired_access_resources items must reference RuntimeResource");
  }
}

function assertCloudRunIdContract(document: Record<string, unknown>): void {
  const schema = componentSchema(document, "CloudRunId");
  if (schema.type !== "string") {
    throw new Error("runs.yaml components.schemas.CloudRunId must be a string schema");
  }
  if ("format" in schema) {
    throw new Error("runs.yaml components.schemas.CloudRunId must stay opaque and must not declare a UUID format");
  }
  if (!String(schema.description ?? "").includes("not as a UUID")) {
    throw new Error("runs.yaml components.schemas.CloudRunId must document that Cloud run ids are not UUIDs");
  }

  const runIdParameter = componentParameter(document, "RunId");
  if (runIdParameter.name !== "run_id" || runIdParameter.in !== "path") {
    throw new Error("runs.yaml components.parameters.RunId must define the run_id path parameter");
  }
  if (!containsRef(runIdParameter, "#/components/schemas/CloudRunId")) {
    throw new Error("runs.yaml components.parameters.RunId must reference components.schemas.CloudRunId");
  }

  const paths = document.paths;
  assertRecord(paths, "paths");
  for (const [path, pathItem] of Object.entries(paths)) {
    if (!path.includes("{run_id}")) continue;
    assertRecord(pathItem, `paths.${path}`);
    for (const [method, operation] of Object.entries(pathItem)) {
      if (!["get", "post", "put", "patch", "delete"].includes(method)) continue;
      assertRecord(operation, `paths.${path}.${method}`);
      const parameters = Array.isArray(operation.parameters) ? operation.parameters : [];
      if (!parameters.some((parameter) => isRecord(parameter) && parameter.$ref === "#/components/parameters/RunId")) {
        throw new Error(`runs.yaml ${method.toUpperCase()} ${path} must use components.parameters.RunId`);
      }
      if (parameters.some((parameter) => isRecord(parameter) && parameter.name === "run_id")) {
        throw new Error(`runs.yaml ${method.toUpperCase()} ${path} must not inline the run_id parameter`);
      }
    }
  }

  assertNoUuidCloudRunIdProperties(document);
}

function operationParameterNames(document: Record<string, unknown>, path: string, method: string): Set<string> {
  const operation = operationObject(document, path, method);
  const parameters = operation.parameters;
  if (!Array.isArray(parameters)) {
    return new Set();
  }
  return new Set(parameters
    .filter(isRecord)
    .map((parameter) => operationParameterName(document, parameter))
    .filter((name): name is string => typeof name === "string"));
}

function operationParameterName(document: Record<string, unknown>, parameter: Record<string, unknown>): string | null {
  if (typeof parameter.name === "string") {
    return parameter.name;
  }
  if (typeof parameter.$ref === "string" && parameter.$ref.startsWith("#/components/parameters/")) {
    const parameterName = parameter.$ref.slice("#/components/parameters/".length);
    const resolved = componentParameter(document, parameterName);
    return typeof resolved.name === "string" ? resolved.name : null;
  }
  return null;
}

function assertOperationId(document: Record<string, unknown>, path: string, method: string, expected: string): void {
  const operation = operationObject(document, path, method);
  if (operation.operationId !== expected) {
    throw new Error(`runs.yaml ${method.toUpperCase()} ${path} must use operationId ${expected}`);
  }
}

function assertOperationParameter(document: Record<string, unknown>, path: string, method: string, parameterName: string): void {
  if (!operationParameterNames(document, path, method).has(parameterName)) {
    throw new Error(`runs.yaml ${method.toUpperCase()} ${path} must declare parameter ${parameterName}`);
  }
}

function assertOperationParameterDescriptionIncludes(
  document: Record<string, unknown>,
  path: string,
  method: string,
  parameterName: string,
  requiredText: string,
): void {
  const parameter = operationParameterObject(document, path, method, parameterName);
  const description = typeof parameter.description === "string" ? parameter.description : "";
  if (!description.includes(requiredText)) {
    throw new Error(`runs.yaml ${method.toUpperCase()} ${path} parameter ${parameterName} description must include ${requiredText}`);
  }
}

function assertOperationResponseHasRef(
  document: Record<string, unknown>,
  path: string,
  method: string,
  status: string,
  expectedRef: string,
): void {
  const operation = operationObject(document, path, method);
  assertRecord(operation.responses, `paths.${path}.${method}.responses`);
  const response = operation.responses[status];
  assertRecord(response, `paths.${path}.${method}.responses.${status}`);
  if (!containsRef(response, expectedRef)) {
    throw new Error(`runs.yaml ${method.toUpperCase()} ${path} response ${status} must reference ${expectedRef}`);
  }
}

function assertOperationResponseHasExactJsonRef(
  document: Record<string, unknown>,
  path: string,
  method: string,
  status: string,
  expectedRef: string,
): void {
  const schema = operationResponseJsonSchema(document, path, method, status);
  if (schema.$ref !== expectedRef) {
    throw new Error(`runs.yaml ${method.toUpperCase()} ${path} response ${status} JSON schema must be ${expectedRef}`);
  }
}

function assertOperationResponseHeader(
  document: Record<string, unknown>,
  path: string,
  method: string,
  status: string,
  headerName: string,
): void {
  const response = operationResponseObject(document, path, method, status);
  assertRecord(response.headers, `paths.${path}.${method}.responses.${status}.headers`);
  if (!(headerName in response.headers)) {
    throw new Error(`runs.yaml ${method.toUpperCase()} ${path} response ${status} must declare header ${headerName}`);
  }
}

function assertOperationRequestHasRef(
  document: Record<string, unknown>,
  path: string,
  method: string,
  expectedRef: string,
): void {
  const operation = operationObject(document, path, method);
  assertRecord(operation.requestBody, `paths.${path}.${method}.requestBody`);
  if (!containsRef(operation.requestBody, expectedRef)) {
    throw new Error(`runs.yaml ${method.toUpperCase()} ${path} request body must reference ${expectedRef}`);
  }
}

function operationRequestJsonSchema(
  document: Record<string, unknown>,
  path: string,
  method: string,
): { schema: Record<string, unknown>; label: string } {
  const operation = operationObject(document, path, method);
  const label = `paths.${path}.${method}.requestBody.content.application/json.schema`;
  assertRecord(operation.requestBody, `paths.${path}.${method}.requestBody`);
  if (operation.requestBody.required !== true) {
    throw new Error(`runs.yaml ${method.toUpperCase()} ${path} request body must be required`);
  }
  assertRecord(operation.requestBody.content, `paths.${path}.${method}.requestBody.content`);
  const json = operation.requestBody.content["application/json"];
  assertRecord(json, `paths.${path}.${method}.requestBody.content.application/json`);
  assertRecord(json.schema, label);
  return resolveLocalSchema(document, json.schema, label);
}

function resolveLocalSchema(
  document: Record<string, unknown>,
  schema: Record<string, unknown>,
  label: string,
): { schema: Record<string, unknown>; label: string } {
  if (typeof schema.$ref !== "string") {
    return { schema, label };
  }
  if (!schema.$ref.startsWith("#/components/schemas/")) {
    throw new Error(`${label} must use an inline schema or local component schema ref`);
  }
  const schemaName = schema.$ref.slice("#/components/schemas/".length);
  return { schema: componentSchema(document, schemaName), label: `components.schemas.${schemaName}` };
}

function operationParameterObject(
  document: Record<string, unknown>,
  path: string,
  method: string,
  parameterName: string,
): Record<string, unknown> {
  const operation = operationObject(document, path, method);
  const parameters = operation.parameters;
  if (!Array.isArray(parameters)) {
    throw new Error(`runs.yaml ${method.toUpperCase()} ${path} must declare parameter ${parameterName}`);
  }
  for (const parameter of parameters) {
    if (!isRecord(parameter)) continue;
    const resolved = typeof parameter.$ref === "string" && parameter.$ref.startsWith("#/components/parameters/")
      ? componentParameter(document, parameter.$ref.slice("#/components/parameters/".length))
      : parameter;
    if (resolved.name === parameterName) {
      return resolved;
    }
  }
  throw new Error(`runs.yaml ${method.toUpperCase()} ${path} must declare parameter ${parameterName}`);
}

function operationObject(document: Record<string, unknown>, path: string, method: string): Record<string, unknown> {
  const pathItem = pathObject(document, path);
  assertRecord(pathItem, `paths.${path}`);
  const operation = pathItem[method];
  assertRecord(operation, `paths.${path}.${method}`);
  return operation;
}

function pathObject(document: Record<string, unknown>, path: string): Record<string, unknown> | null {
  const paths = document.paths;
  assertRecord(paths, "paths");
  const pathItem = paths[path];
  if (pathItem === undefined) {
    return null;
  }
  assertRecord(pathItem, `paths.${path}`);
  return pathItem;
}

function operationResponseObject(document: Record<string, unknown>, path: string, method: string, status: string): Record<string, unknown> {
  const operation = operationObject(document, path, method);
  assertRecord(operation.responses, `paths.${path}.${method}.responses`);
  const response = operation.responses[status];
  assertRecord(response, `paths.${path}.${method}.responses.${status}`);
  return response;
}

function operationResponseJsonSchema(document: Record<string, unknown>, path: string, method: string, status: string): Record<string, unknown> {
  const response = operationResponseObject(document, path, method, status);
  assertRecord(response.content, `paths.${path}.${method}.responses.${status}.content`);
  const json = response.content["application/json"];
  assertRecord(json, `paths.${path}.${method}.responses.${status}.content.application/json`);
  assertRecord(json.schema, `paths.${path}.${method}.responses.${status}.content.application/json.schema`);
  return json.schema;
}

function assertOperationResponseEnvelopeProperty(
  document: Record<string, unknown>,
  path: string,
  method: string,
  status: string,
  propertyName: string,
  expectedRef: string,
  shape: "array" | "ref",
): void {
  const label = `paths.${path}.${method}.responses.${status}.content.application/json.schema`;
  const schema = operationResponseJsonSchema(document, path, method, status);
  if (schema.type !== "object") {
    throw new Error(`runs.yaml ${method.toUpperCase()} ${path} response ${status} must use an object envelope`);
  }
  if (schema.additionalProperties !== false) {
    throw new Error(`runs.yaml ${method.toUpperCase()} ${path} response ${status} must forbid additional response envelope properties`);
  }
  assertRecord(schema.properties, `${label}.properties`);
  const propertyNames = Object.keys(schema.properties);
  if (propertyNames.length !== 1 || propertyNames[0] !== propertyName) {
    throw new Error(`runs.yaml ${method.toUpperCase()} ${path} response ${status} must expose only ${propertyName}`);
  }
  const required = schema.required;
  if (!Array.isArray(required) || required.length !== 1 || required[0] !== propertyName) {
    throw new Error(`runs.yaml ${method.toUpperCase()} ${path} response ${status} must require only ${propertyName}`);
  }
  const property = schema.properties[propertyName];
  assertRecord(property, `${label}.properties.${propertyName}`);
  if (shape === "array") {
    if (property.type !== "array") {
      throw new Error(`runs.yaml ${method.toUpperCase()} ${path} response ${status} ${propertyName} must be an array`);
    }
    assertRecord(property.items, `${label}.properties.${propertyName}.items`);
    if (property.items.$ref !== expectedRef) {
      throw new Error(`runs.yaml ${method.toUpperCase()} ${path} response ${status} ${propertyName} items must reference ${expectedRef}`);
    }
    return;
  }
  if (property.$ref !== expectedRef) {
    throw new Error(`runs.yaml ${method.toUpperCase()} ${path} response ${status} ${propertyName} must reference ${expectedRef}`);
  }
}

function containsRef(value: unknown, expectedRef: string): boolean {
  if (Array.isArray(value)) {
    return value.some((item) => containsRef(item, expectedRef));
  }
  if (!isRecord(value)) {
    return false;
  }
  if (value.$ref === expectedRef) {
    return true;
  }
  return Object.values(value).some((child) => containsRef(child, expectedRef));
}

function componentParameter(document: Record<string, unknown>, parameterName: string): Record<string, unknown> {
  assertRecord(document.components, "components");
  assertRecord(document.components.parameters, "components.parameters");
  const parameter = document.components.parameters[parameterName];
  assertRecord(parameter, `components.parameters.${parameterName}`);
  return parameter;
}

function componentSchema(document: Record<string, unknown>, schemaName: string): Record<string, unknown> {
  assertRecord(document.components, "components");
  assertRecord(document.components.schemas, "components.schemas");
  const schema = document.components.schemas[schemaName];
  assertRecord(schema, `components.schemas.${schemaName}`);
  return schema;
}

function assertNoUuidCloudRunIdProperties(value: unknown, path = "#"): void {
  if (Array.isArray(value)) {
    value.forEach((item, index) => assertNoUuidCloudRunIdProperties(item, `${path}/${index}`));
    return;
  }
  if (!isRecord(value)) {
    return;
  }
  for (const [key, child] of Object.entries(value)) {
    if ((key === "run_id" || key === "cloud_run_id") && isRecord(child) && child.format === "uuid") {
      throw new Error(`runs.yaml ${path}/${escapePointer(key)} must reference CloudRunId or otherwise stay opaque, not format: uuid`);
    }
    assertNoUuidCloudRunIdProperties(child, `${path}/${escapePointer(key)}`);
  }
}

function schemaProperty(schema: Record<string, unknown>, propertyName: string, label: string): Record<string, unknown> {
  assertRecord(schema.properties, `${label}.properties`);
  const property = schema.properties[propertyName];
  assertRecord(property, `${label}.properties.${propertyName}`);
  return property;
}

function assertSchemaPropertyDescriptionIncludes(
  schema: Record<string, unknown>,
  propertyName: string,
  label: string,
  requiredText: string,
): void {
  const property = schemaProperty(schema, propertyName, label);
  const description = typeof property.description === "string" ? property.description : "";
  if (!description.includes(requiredText)) {
    throw new Error(`${label}.${propertyName} description must include ${requiredText}`);
  }
}

function assertSchemaPropertyRef(
  schema: Record<string, unknown>,
  propertyName: string,
  label: string,
  expectedRef: string,
): void {
  const property = schemaProperty(schema, propertyName, label);
  if (property.$ref !== expectedRef) {
    throw new Error(`${label}.${propertyName} must use ${expectedRef}`);
  }
}

function assertSchemaRequired(document: Record<string, unknown>, schemaName: string, fields: readonly string[]): void {
  assertSchemaRequiredFields(componentSchema(document, schemaName), `components.schemas.${schemaName}`, fields);
}

function assertSchemaAbsent(document: Record<string, unknown>, schemaName: string): void {
  assertRecord(document.components, "components");
  assertRecord(document.components.schemas, "components.schemas");
  if (Object.prototype.hasOwnProperty.call(document.components.schemas, schemaName)) {
    throw new Error(`components.schemas.${schemaName} must not be present`);
  }
}

function assertSchemaRequiredFields(schema: Record<string, unknown>, label: string, fields: readonly string[]): void {
  const required = schema.required;
  if (!Array.isArray(required)) {
    throw new Error(`${label} must declare required fields`);
  }
  for (const field of fields) {
    if (!required.includes(field)) {
      throw new Error(`${label} must require ${field}`);
    }
  }
}

function assertSchemaPropertiesPresent(document: Record<string, unknown>, schemaName: string, fields: string[]): void {
  const schema = componentSchema(document, schemaName);
  assertRecord(schema.properties, `components.schemas.${schemaName}.properties`);
  for (const field of fields) {
    if (!(field in schema.properties)) {
      throw new Error(`components.schemas.${schemaName} must expose property ${field}`);
    }
  }
}

function assertSchemaPropertiesAbsent(document: Record<string, unknown>, schemaName: string, fields: string[]): void {
  const schema = componentSchema(document, schemaName);
  assertRecord(schema.properties, `components.schemas.${schemaName}.properties`);
  for (const field of fields) {
    if (field in schema.properties) {
      throw new Error(`components.schemas.${schemaName} must not expose unsupported property ${field}`);
    }
  }
}

function validateRefs(absPath: string, value: unknown, path = "#"): void {
  if (Array.isArray(value)) {
    value.forEach((item, index) => validateRefs(absPath, item, `${path}/${index}`));
    return;
  }
  if (!isRecord(value)) {
    return;
  }
  if (typeof value.$ref === "string") {
    resolveRef(absPath, value.$ref, path);
  }
  for (const [key, child] of Object.entries(value)) {
    validateRefs(absPath, child, `${path}/${escapePointer(key)}`);
  }
}

function resolveRef(fromAbsPath: string, ref: string, atPath: string): void {
  const [rawFile, rawPointer = ""] = ref.split("#", 2);
  const targetPath = rawFile
    ? normalize(join(dirname(fromAbsPath), rawFile))
    : fromAbsPath;
  const target = documents.get(targetPath);
  if (!target) {
    throwRefError(fromAbsPath, atPath, ref, `target file is not part of OpenAPI bundle: ${relative(targetPath)}`);
  }
  if (!rawPointer) {
    return;
  }
  if (!rawPointer.startsWith("/")) {
    throwRefError(fromAbsPath, atPath, ref, "fragment must be a JSON pointer");
  }
  let current: unknown = target;
  for (const segment of rawPointer.slice(1).split("/").map(unescapePointer)) {
    if (isRecord(current) && segment in current) {
      current = current[segment];
      continue;
    }
    throwRefError(fromAbsPath, atPath, ref, `missing JSON pointer segment: ${segment}`);
  }
}

function throwRefError(fromAbsPath: string, atPath: string, ref: string, details: string): never {
  throw new Error(`${relative(fromAbsPath)} ${atPath} has invalid $ref ${ref}: ${details}`);
}

function assertRecord(value: unknown, label: string): asserts value is Record<string, unknown> {
  if (!isRecord(value)) {
    throw new Error(`${label} must be an object`);
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function escapePointer(value: string): string {
  return value.replace(/~/g, "~0").replace(/\//g, "~1");
}

function unescapePointer(value: string): string {
  return value.replace(/~1/g, "/").replace(/~0/g, "~");
}

function relative(absPath: string): string {
  return absPath.startsWith(rootDir) ? absPath.slice(rootDir.length) : absPath;
}

function basename(absPath: string): string {
  return absPath.split("/").at(-1) ?? absPath;
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

#!/usr/bin/env bun
import { dirname, join, normalize } from "node:path";
import { parse } from "yaml";

const rootDir = new URL("..", import.meta.url).pathname;
const openApiDir = join(rootDir, "api", "openapi");
const files = [
  "common.yaml",
  "registry.yaml",
  "drafts.yaml",
  "imports.yaml",
  "runs.yaml",
  "analysis.yaml",
  "observability.yaml",
];

const documents = new Map<string, unknown>();
let failures = 0;
const MAX_CLOUD_RESOURCE_INT = 2_147_483_647;
const MAX_CLOUD_RESOURCE_INT_STRING_PATTERN = "^(?:[1-9][0-9]{0,8}|1[0-9]{9}|20[0-9]{8}|21[0-3][0-9]{7}|214[0-6][0-9]{6}|2147[0-3][0-9]{5}|21474[0-7][0-9]{4}|214748[0-2][0-9]{3}|2147483[0-5][0-9]{2}|21474836[0-3][0-9]|214748364[0-7])$";

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
  } catch (error) {
    failures += 1;
    console.error(`OpenAPI ref validation failed: ${message(error)}`);
  }
}

for (const [absPath, document] of documents) {
  try {
    validateSemanticContracts(absPath, document);
  } catch (error) {
    failures += 1;
    console.error(`OpenAPI semantic validation failed: ${message(error)}`);
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

function validateSemanticContracts(absPath: string, document: unknown): void {
  if (basename(absPath) !== "runs.yaml") {
    return;
  }
  assertJsonPointer(
    document,
    "/paths/~1v1~1runs/post/requestBody/content/application~1json/schema/properties/runtime_options/$ref",
    "#/components/schemas/CreateRunRuntimeOptions",
  );
  assertJsonPointer(
    document,
    "/components/schemas/CloudRun/properties/runtime_options/$ref",
    "#/components/schemas/CreateRunRuntimeOptions",
  );
  assertJsonPointer(
    document,
    "/components/schemas/WorkerCapabilities/properties/executors/items/$ref",
    "#/components/schemas/CloudExecutorOption",
  );
  assertJsonPointer(
    document,
    "/components/schemas/WorkerCapabilities/properties/resources/items/minLength",
    1,
  );
  for (const pointer of [
    "/components/schemas/WorkerCapabilities/properties/cpu_count/maximum",
    "/components/schemas/WorkerCapabilities/properties/memory_mb/maximum",
    "/components/schemas/WorkerCapabilities/properties/disk_mb/maximum",
    "/components/schemas/RunRequirements/properties/cpu_count/maximum",
    "/components/schemas/RunRequirements/properties/memory_mb/maximum",
    "/components/schemas/RunRequirements/properties/disk_mb/maximum",
    "/components/schemas/RunRequirements/properties/timeout_ms/maximum",
    "/components/schemas/RunRequirements/properties/max_parallel_trials/maximum",
    "/components/schemas/PositiveIntegerOrString/oneOf/0/maximum",
  ]) {
    assertJsonPointer(document, pointer, MAX_CLOUD_RESOURCE_INT);
  }
  assertJsonPointer(
    document,
    "/components/schemas/PositiveIntegerOrString/oneOf/1/pattern",
    MAX_CLOUD_RESOURCE_INT_STRING_PATTERN,
  );
  const graderStrategyEnum = jsonPointer(
    document,
    "/components/schemas/CreateRunTrialRuntimeOptions/properties/grader/properties/strategy/enum",
  );
  if (
    !Array.isArray(graderStrategyEnum)
    || graderStrategyEnum.includes("host")
    || !["none", "in_task_runtime", "injected", "separate"].every((strategy) => graderStrategyEnum.includes(strategy))
  ) {
    throw new Error("runs.yaml CreateRunTrialRuntimeOptions grader.strategy must enumerate Cloud-supported non-host strategies");
  }
  for (const pointer of [
    "/components/schemas/CreateRunTrialRuntimeOptions/properties/agent/properties/image/$ref",
    "/components/schemas/CreateRunTrialRuntimeOptions/properties/grader/properties/separate/properties/image/$ref",
    "/components/schemas/CreateRunTrialRuntimeOptions/properties/task/properties/workspace/properties/image/$ref",
  ]) {
    assertJsonPointer(document, pointer, "#/components/schemas/CloudRuntimeImageRef");
  }
  for (const pointer of [
    "/components/schemas/CreateRunTrialRuntimeOptions/properties/agent/properties/sidecars/not",
    "/components/schemas/CreateRunTrialRuntimeOptions/properties/agent/properties/ephemerals/not",
    "/components/schemas/CreateRunTrialRuntimeOptions/properties/grader/properties/sidecars/not",
    "/components/schemas/CreateRunTrialRuntimeOptions/properties/grader/properties/ephemerals/not",
  ]) {
    const value = jsonPointer(document, pointer);
    if (!isRecord(value) || Object.keys(value).length !== 0) {
      throw new Error("runs.yaml CreateRunTrialRuntimeOptions must forbid nested trial_runtime sidecars/ephemerals");
    }
  }
  for (const pointer of [
    "/components/schemas/RunRequirements/properties/network_perimeter/properties/default/enum",
    "/components/schemas/RunRequirements/properties/network_perimeter/properties/task_sandbox/enum",
    "/components/schemas/RunRequirements/properties/network_perimeter/properties/agent/enum",
  ]) {
    const value = jsonPointer(document, pointer);
    if (!Array.isArray(value) || !value.includes("allowlist_enforced")) {
      throw new Error(`runs.yaml ${pointer} must include allowlist_enforced`);
    }
  }
}

function assertJsonPointer(document: unknown, pointer: string, expected: unknown): void {
  const actual = jsonPointer(document, pointer);
  if (actual !== expected) {
    throw new Error(`runs.yaml ${pointer} expected ${String(expected)}, got ${String(actual)}`);
  }
}

function jsonPointer(document: unknown, pointer: string): unknown {
  let current = document;
  for (const segment of pointer.slice(1).split("/").map(unescapePointer)) {
    if (Array.isArray(current) && /^\d+$/.test(segment)) {
      const index = Number.parseInt(segment, 10);
      if (index in current) {
        current = current[index];
        continue;
      }
      return undefined;
    }
    if (isRecord(current) && segment in current) {
      current = current[segment];
      continue;
    }
    return undefined;
  }
  return current;
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

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

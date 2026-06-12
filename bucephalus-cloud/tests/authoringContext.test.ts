import { describe, expect, test } from "bun:test";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import * as tar from "tar";
import { extractAuthoringContextArchive } from "../src/imports/authoringContext";

describe("authoring context archive extraction", () => {
  test("accepts normal explicit directory entries and builds an entrypoint under them", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-authoring-context-"));
    try {
      const sourceDir = join(root, "source");
      await mkdir(join(sourceDir, "experiments/peter"), { recursive: true });
      await writeFile(join(sourceDir, "experiments/peter/experiment.yaml"), "experiment:\n  name: Peter\n");
      const archivePath = join(root, "context.tgz");
      await tar.c({ gzip: true, cwd: sourceDir, file: archivePath }, ["experiments"]);

      const inspection = await extractAuthoringContextArchive({
        archivePath,
        workDir: join(root, "work"),
        entrypoint: "experiments/peter/experiment.yaml",
      });

      expect(inspection.entrypoint).toBe("experiments/peter/experiment.yaml");
      expect(inspection.entries).toBeGreaterThan(1);
      await expect(readFile(join(root, "work", "experiments/peter/experiment.yaml"), "utf8"))
        .resolves
        .toContain("Peter");
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("rejects archives where a later entry is nested under a file path", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-authoring-context-conflict-"));
    try {
      const archivePath = join(root, "conflict.tar");
      await writeManualTar(archivePath, [
        { path: "experiments", type: "file", data: "not a directory" },
        { path: "experiments/peter/experiment.yaml", type: "file", data: "experiment: {}\n" },
      ]);

      await expect(extractAuthoringContextArchive({
        archivePath,
        workDir: join(root, "work"),
        entrypoint: "experiments/peter/experiment.yaml",
      })).rejects.toThrow("nested under a file path");
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("rejects generated and dependency directories that the hosted CLI excludes", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-authoring-context-blocked-dir-"));
    try {
      const archivePath = join(root, "blocked.tar");
      await writeManualTar(archivePath, [
        { path: "experiment.yaml", type: "file", data: "experiment: {}\n" },
        { path: "node_modules/pkg/index.js", type: "file", data: "not for cloud\n" },
      ]);

      await expect(extractAuthoringContextArchive({
        archivePath,
        workDir: join(root, "work"),
        entrypoint: "experiment.yaml",
      })).rejects.toThrow("blocked path");
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("rejects common local credential material that the hosted CLI excludes", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-authoring-context-credentials-"));
    try {
      const archivePath = join(root, "credentials.tar");
      await writeManualTar(archivePath, [
        { path: "experiment.yaml", type: "file", data: "experiment: {}\n" },
        { path: ".npmrc", type: "file", data: "//registry.example/:_authToken=oops\n" },
      ]);

      await expect(extractAuthoringContextArchive({
        archivePath,
        workDir: join(root, "work"),
        entrypoint: "experiment.yaml",
      })).rejects.toThrow("blocked path");
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("rejects credential directories that the hosted CLI excludes", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-authoring-context-credential-dir-"));
    try {
      const archivePath = join(root, "credential-dir.tar");
      await writeManualTar(archivePath, [
        { path: "experiment.yaml", type: "file", data: "experiment: {}\n" },
        { path: ".ssh/id_ed25519", type: "file", data: "private key\n" },
      ]);

      await expect(extractAuthoringContextArchive({
        archivePath,
        workDir: join(root, "work"),
        entrypoint: "experiment.yaml",
      })).rejects.toThrow("blocked path");
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("allows a normal file whose basename matches an excluded generated directory", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-authoring-context-target-file-"));
    try {
      const archivePath = join(root, "target-file.tar");
      await writeManualTar(archivePath, [
        { path: "experiment.yaml", type: "file", data: "experiment: {}\n" },
        { path: "target", type: "file", data: "case label\n" },
      ]);

      const inspection = await extractAuthoringContextArchive({
        archivePath,
        workDir: join(root, "work"),
        entrypoint: "experiment.yaml",
      });

      expect(inspection.entrypoint).toBe("experiment.yaml");
      await expect(readFile(join(root, "work", "target"), "utf8"))
        .resolves
        .toBe("case label\n");
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });
});

async function writeManualTar(
  path: string,
  entries: Array<{ path: string; type: "file" | "directory"; data?: string }>,
): Promise<void> {
  const blocks: Buffer[] = [];
  for (const entry of entries) {
    const data = Buffer.from(entry.data ?? "", "utf8");
    blocks.push(tarHeader({
      path: entry.path,
      typeflag: entry.type === "directory" ? "5" : "0",
      size: entry.type === "directory" ? 0 : data.byteLength,
    }));
    if (entry.type === "file") {
      blocks.push(data);
      const padding = data.byteLength % 512 === 0 ? 0 : 512 - (data.byteLength % 512);
      if (padding > 0) {
        blocks.push(Buffer.alloc(padding));
      }
    }
  }
  blocks.push(Buffer.alloc(1024));
  await writeFile(path, Buffer.concat(blocks));
}

function tarHeader(input: { path: string; typeflag: string; size: number }): Buffer {
  const header = Buffer.alloc(512);
  writeString(header, input.path, 0, 100);
  writeOctal(header, 0o644, 100, 8);
  writeOctal(header, 0, 108, 8);
  writeOctal(header, 0, 116, 8);
  writeOctal(header, input.size, 124, 12);
  writeOctal(header, 0, 136, 12);
  header.fill(0x20, 148, 156);
  writeString(header, input.typeflag, 156, 1);
  writeString(header, "ustar", 257, 6);
  writeString(header, "00", 263, 2);
  let checksum = 0;
  for (const byte of header) {
    checksum += byte;
  }
  writeString(header, checksum.toString(8).padStart(6, "0"), 148, 6);
  header[154] = 0;
  header[155] = 0x20;
  return header;
}

function writeString(buffer: Buffer, value: string, offset: number, length: number): void {
  buffer.write(value, offset, length, "utf8");
}

function writeOctal(buffer: Buffer, value: number, offset: number, length: number): void {
  const encoded = value.toString(8).padStart(length - 1, "0");
  buffer.write(encoded, offset, length - 1, "ascii");
  buffer[offset + length - 1] = 0;
}

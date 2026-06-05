#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VERSION="${BUCEPHALUS_RELEASE_VERSION:-}"
OUT_DIR="${BUCEPHALUS_RELEASE_OUT_DIR:-${ROOT_DIR}/dist/releases}"
TARGET="${BUCEPHALUS_RELEASE_TARGET:-}"
CORE_BIN_INPUT="${BUCEPHALUS_RELEASE_CORE_BIN:-}"
ARCHIVE_BASENAME=""
RUNTIME_BUILD_DIR=""

usage() {
  cat <<'USAGE'
Usage: scripts/release/build-buc-release.sh --version <version> [--out <dir>] [--target <rust-target>] [--core-bin <path>]

Builds a Bucephalus release directory containing:
  - bin/bucephalus
  - bin/bucephalus-worker-runner
  - bucephalus-cloud worker/controller/API bundle
  - release input lockfiles used to build the bundle
  - migrations, OpenAPI specs, and deployment contracts
  - release-manifest.json
  - SHA256SUMS
  - a tar.gz archive

Set BUCEPHALUS_RELEASE_SKIP_CLOUD_CHECKS=true only after an earlier CI gate has
already run the Cloud install, typecheck, tests, OpenAPI parse, and migrations.
Use --core-bin only with a prebuilt bucephalus binary from a verified matching
Core release artifact.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      VERSION="${2:-}"
      shift 2
      ;;
    --out)
      OUT_DIR="${2:-}"
      shift 2
      ;;
    --target)
      TARGET="${2:-}"
      shift 2
      ;;
    --core-bin)
      CORE_BIN_INPUT="${2:-}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ -z "${VERSION}" ]]; then
  echo "--version or BUCEPHALUS_RELEASE_VERSION is required" >&2
  exit 2
fi

require_command() {
  local name="$1"
  if ! command -v "${name}" >/dev/null 2>&1; then
    echo "required command not found: ${name}" >&2
    exit 2
  fi
}

cleanup() {
  if [[ -n "${RUNTIME_BUILD_DIR}" ]]; then
    rm -rf "${RUNTIME_BUILD_DIR}"
  fi
}
trap cleanup EXIT

sha256_file() {
  local file="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "${file}" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "${file}" | awk '{print $1}'
  else
    echo "sha256sum or shasum is required" >&2
    exit 2
  fi
}

sha256_text() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 | awk '{print $1}'
  else
    echo "sha256sum or shasum is required" >&2
    exit 2
  fi
}

sha256_tree() {
  local dir="$1"
  (
    cd "${dir}"
    find . -type f | sort | while read -r file; do
      digest="$(sha256_file "${file}")"
      printf "%s  %s\n" "${digest}" "${file#./}"
    done
  ) | sha256_text
}

require_command cargo
require_command bun
require_command git
require_command install
require_command tar

CARGO_BUILD_SUBCOMMAND="${BUCEPHALUS_RELEASE_CARGO_BUILD_SUBCOMMAND:-build}"

GIT_SHA="$(git -C "${ROOT_DIR}" rev-parse HEAD)"
GIT_DIRTY="false"
if [[ -n "$(git -C "${ROOT_DIR}" status --porcelain)" ]]; then
  GIT_DIRTY="true"
fi
if [[ "${GIT_DIRTY}" == "true" && "${BUCEPHALUS_RELEASE_ALLOW_DIRTY:-false}" != "true" ]]; then
  echo "worktree is dirty; set BUCEPHALUS_RELEASE_ALLOW_DIRTY=true for local smoke builds" >&2
  exit 2
fi
BUILD_DATE="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
HOST_ARCH="$(uname -m)"
HOST_OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
TARGET_LABEL="${TARGET:-${HOST_ARCH}-${HOST_OS}}"
RELEASE_NAME="bucephalus-${VERSION}-${TARGET_LABEL}"
RELEASE_DIR="${OUT_DIR}/${RELEASE_NAME}"
ARCHIVE_BASENAME="${RELEASE_NAME}.tar.gz"
ARCHIVE_PATH="${OUT_DIR}/${ARCHIVE_BASENAME}"

rm -rf "${RELEASE_DIR}" "${ARCHIVE_PATH}"
mkdir -p "${RELEASE_DIR}/bin" "${RELEASE_DIR}/bucephalus-cloud" "${RELEASE_DIR}/release-inputs"

if [[ -n "${CORE_BIN_INPUT}" ]]; then
  echo "== Using prebuilt bucephalus ${VERSION} =="
  if [[ ! -f "${CORE_BIN_INPUT}" ]]; then
    echo "--core-bin does not exist: ${CORE_BIN_INPUT}" >&2
    exit 2
  fi
  CORE_BIN="${CORE_BIN_INPUT}"
else
  echo "== Building bucephalus ${VERSION} =="
  if [[ -n "${TARGET}" ]]; then
    cargo "${CARGO_BUILD_SUBCOMMAND}" --manifest-path "${ROOT_DIR}/Cargo.toml" --release --bin bucephalus --target "${TARGET}"
    CORE_BIN="${ROOT_DIR}/target/${TARGET}/release/bucephalus"
  else
    cargo "${CARGO_BUILD_SUBCOMMAND}" --manifest-path "${ROOT_DIR}/Cargo.toml" --release --bin bucephalus
    CORE_BIN="${ROOT_DIR}/target/release/bucephalus"
  fi
fi

install -m 0755 "${CORE_BIN}" "${RELEASE_DIR}/bin/bucephalus"

echo "== Building bucephalus-worker-runner ${VERSION} =="
if [[ -n "${TARGET}" ]]; then
  cargo "${CARGO_BUILD_SUBCOMMAND}" --manifest-path "${ROOT_DIR}/Cargo.toml" --release --bin bucephalus-worker-runner --target "${TARGET}"
  WORKER_RUNNER_BIN="${ROOT_DIR}/target/${TARGET}/release/bucephalus-worker-runner"
else
  cargo "${CARGO_BUILD_SUBCOMMAND}" --manifest-path "${ROOT_DIR}/Cargo.toml" --release --bin bucephalus-worker-runner
  WORKER_RUNNER_BIN="${ROOT_DIR}/target/release/bucephalus-worker-runner"
fi
install -m 0755 "${WORKER_RUNNER_BIN}" "${RELEASE_DIR}/bin/bucephalus-worker-runner"
install -m 0644 "${ROOT_DIR}/Cargo.lock" "${RELEASE_DIR}/release-inputs/Cargo.lock"

cat > "${RELEASE_DIR}/.dockerignore" <<'EOF'
# Image build context guard for verified Bucephalus Cloud release bundles.
.git
.git/
**/.git
**/.git/
.env
*.env
**/.env
**/*.env
*.env.example
**/*.env.example
gha-creds-*.json
**/gha-creds-*.json
node_modules
node_modules/
**/node_modules
**/node_modules/
.terraform
.terraform/
**/.terraform
**/.terraform/
*.tfstate
*.tfstate.*
**/*.tfstate
**/*.tfstate.*
image-build
image-build/
**/image-build
**/image-build/
*.metadata.json
*.iid
EOF

if [[ "${BUCEPHALUS_RELEASE_SKIP_CLOUD_CHECKS:-false}" == "true" ]]; then
  echo "== Cloud validation skipped: trusted earlier CI gates =="
else
  echo "== Preparing cloud bundle =="
  (
    cd "${ROOT_DIR}/bucephalus-cloud"
    bun install --frozen-lockfile
    bun run typecheck
    bun test
  )
fi

for path in \
  package.json \
  package.runtime.json \
  bun.lock \
  bun.runtime.lock \
  tsconfig.json \
  docker-compose.yml \
  scripts \
  src \
  api \
  db \
  images \
  deploy \
  infra
do
  cp -R "${ROOT_DIR}/bucephalus-cloud/${path}" "${RELEASE_DIR}/bucephalus-cloud/${path}"
done

echo "== Building cloud runtime bundles =="
RUNTIME_BUILD_DIR="$(mktemp -d)"
(
  cp "${RELEASE_DIR}/bucephalus-cloud/package.runtime.json" "${RUNTIME_BUILD_DIR}/package.json"
  cp "${RELEASE_DIR}/bucephalus-cloud/bun.runtime.lock" "${RUNTIME_BUILD_DIR}/bun.lock"
  cp -R "${RELEASE_DIR}/bucephalus-cloud/src" "${RUNTIME_BUILD_DIR}/src"
  cd "${RUNTIME_BUILD_DIR}"
  bun install --frozen-lockfile --production
  bun build \
    --target=bun \
    --outdir "${RELEASE_DIR}/bucephalus-cloud/runtime-dist" \
    src/server.ts \
    src/worker.ts \
    src/db/migrate.ts \
    src/poolController.ts \
    src/secretResolver.ts
)
rm -rf "${RUNTIME_BUILD_DIR}"
RUNTIME_BUILD_DIR=""

echo "== Release size report =="
RELEASE_DIR="${RELEASE_DIR}" bun -e '
import { createHash } from "node:crypto";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";

const releaseDir = process.env.RELEASE_DIR;
const includePaths = [
  "bin",
  "release-inputs",
  "bucephalus-cloud/runtime-dist",
  "bucephalus-cloud/db",
  "bucephalus-cloud/images",
  "bucephalus-cloud/src",
  "bucephalus-cloud/api",
  "bucephalus-cloud/deploy",
  "bucephalus-cloud/infra",
];

function sha256File(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function collect(root, relPath) {
  const path = join(root, relPath);
  const stat = statSync(path);
  if (stat.isFile()) {
    return {
      path: relPath,
      size_bytes: stat.size,
      file_count: 1,
      files: [{
        path: relPath,
        size_bytes: stat.size,
        sha256: sha256File(path),
      }],
    };
  }
  const files = [];
  function walk(current) {
    for (const name of readdirSync(current).sort()) {
      const child = join(current, name);
      const childStat = statSync(child);
      if (childStat.isDirectory()) {
        walk(child);
      } else if (childStat.isFile()) {
        const childRel = relative(root, child).split("\\").join("/");
        files.push({
          path: childRel,
          size_bytes: childStat.size,
          sha256: sha256File(child),
        });
      }
    }
  }
  walk(path);
  return {
    path: relPath,
    size_bytes: files.reduce((sum, file) => sum + file.size_bytes, 0),
    file_count: files.length,
    files,
  };
}

const sections = includePaths.map((path) => collect(releaseDir, path));
const files = sections.flatMap((section) => section.files);
const report = {
  schema_version: "bucephalus_release_size_report_v1",
  generated_at: new Date().toISOString(),
  total: {
    size_bytes: files.reduce((sum, file) => sum + file.size_bytes, 0),
    file_count: files.length,
  },
  sections,
};
await Bun.write(join(releaseDir, "release-size-report.json"), `${JSON.stringify(report, null, 2)}\n`);
'

CORE_SHA="$(sha256_file "${RELEASE_DIR}/bin/bucephalus")"
WORKER_RUNNER_SHA="$(sha256_file "${RELEASE_DIR}/bin/bucephalus-worker-runner")"
SIZE_REPORT_SHA="$(sha256_file "${RELEASE_DIR}/release-size-report.json")"
DOCKERIGNORE_SHA="$(sha256_file "${RELEASE_DIR}/.dockerignore")"
CARGO_LOCK_SHA="$(sha256_file "${RELEASE_DIR}/release-inputs/Cargo.lock")"
CLOUD_LOCK_SHA="$(sha256_file "${RELEASE_DIR}/bucephalus-cloud/bun.lock")"
CLOUD_RUNTIME_LOCK_SHA="$(sha256_file "${RELEASE_DIR}/bucephalus-cloud/bun.runtime.lock")"
CLOUD_PACKAGE_SHA="$(sha256_file "${RELEASE_DIR}/bucephalus-cloud/package.json")"
CLOUD_RUNTIME_PACKAGE_SHA="$(sha256_file "${RELEASE_DIR}/bucephalus-cloud/package.runtime.json")"
CLOUD_SRC_TREE_SHA="$(sha256_tree "${RELEASE_DIR}/bucephalus-cloud/src")"
CLOUD_DB_TREE_SHA="$(sha256_tree "${RELEASE_DIR}/bucephalus-cloud/db/migrations")"
CLOUD_OPENAPI_TREE_SHA="$(sha256_tree "${RELEASE_DIR}/bucephalus-cloud/api/openapi")"
CLOUD_IMAGES_TREE_SHA="$(sha256_tree "${RELEASE_DIR}/bucephalus-cloud/images")"
CLOUD_RUNTIME_DIST_TREE_SHA="$(sha256_tree "${RELEASE_DIR}/bucephalus-cloud/runtime-dist")"

cat > "${RELEASE_DIR}/release-manifest.json" <<EOF
{
  "schema_version": "bucephalus_release_v1",
  "version": "${VERSION}",
  "git_sha": "${GIT_SHA}",
  "git_dirty": ${GIT_DIRTY},
  "build_date": "${BUILD_DATE}",
  "target": "${TARGET_LABEL}",
  "source_inputs": {
    "lockfiles": {
      "cargo": {
        "path": "release-inputs/Cargo.lock",
        "sha256": "${CARGO_LOCK_SHA}"
      },
      "cloud_bun": {
        "path": "bucephalus-cloud/bun.lock",
        "sha256": "${CLOUD_LOCK_SHA}"
      },
      "cloud_runtime_bun": {
        "path": "bucephalus-cloud/bun.runtime.lock",
        "sha256": "${CLOUD_RUNTIME_LOCK_SHA}"
      }
    },
    "cloud_package": {
      "path": "bucephalus-cloud/package.json",
      "sha256": "${CLOUD_PACKAGE_SHA}"
    },
    "cloud_runtime_package": {
      "path": "bucephalus-cloud/package.runtime.json",
      "sha256": "${CLOUD_RUNTIME_PACKAGE_SHA}"
    },
    "image_context_ignore": {
      "path": ".dockerignore",
      "sha256": "${DOCKERIGNORE_SHA}"
    },
    "content_sets": {
      "cloud_src": {
        "path": "bucephalus-cloud/src",
        "tree_sha256": "${CLOUD_SRC_TREE_SHA}"
      },
      "cloud_migrations": {
        "path": "bucephalus-cloud/db/migrations",
        "tree_sha256": "${CLOUD_DB_TREE_SHA}"
      },
      "cloud_openapi": {
        "path": "bucephalus-cloud/api/openapi",
        "tree_sha256": "${CLOUD_OPENAPI_TREE_SHA}"
      },
      "cloud_images": {
        "path": "bucephalus-cloud/images",
        "tree_sha256": "${CLOUD_IMAGES_TREE_SHA}"
      },
      "cloud_runtime_dist": {
        "path": "bucephalus-cloud/runtime-dist",
        "tree_sha256": "${CLOUD_RUNTIME_DIST_TREE_SHA}"
      }
    }
  },
  "artifacts": {
    "core_binary": {
      "path": "bin/bucephalus",
      "sha256": "${CORE_SHA}"
    },
    "worker_runner_binary": {
      "path": "bin/bucephalus-worker-runner",
      "sha256": "${WORKER_RUNNER_SHA}"
    },
    "size_report": {
      "path": "release-size-report.json",
      "sha256": "${SIZE_REPORT_SHA}"
    },
    "cloud_bundle": {
      "path": "bucephalus-cloud",
      "runtime": "bun",
      "entrypoints": {
        "api": "bun run start",
        "worker": "bun run worker",
        "pool_controller": "bun run pool-controller",
        "migrations": "bun run db:migrate"
      }
    }
  },
  "schemas": {
    "sealed_package": "sealed_run_package_v2",
    "release": "bucephalus_release_v1"
  }
}
EOF

echo "== Checksums =="
(
  cd "${RELEASE_DIR}"
  find . -type f ! -name SHA256SUMS | sort | while read -r file; do
    digest="$(sha256_file "${file}")"
    printf "%s  %s\n" "${digest}" "${file#./}"
  done > SHA256SUMS
)

echo "== Archive =="
mkdir -p "${OUT_DIR}"
(
  cd "${OUT_DIR}"
  tar -czf "${ARCHIVE_BASENAME}" "${RELEASE_NAME}"
)

ARCHIVE_SHA="$(sha256_file "${ARCHIVE_PATH}")"
cat > "${OUT_DIR}/${ARCHIVE_BASENAME}.sha256" <<EOF
${ARCHIVE_SHA}  ${ARCHIVE_BASENAME}
EOF

echo "release_dir=${RELEASE_DIR}"
echo "archive=${ARCHIVE_PATH}"
echo "archive_sha256=${ARCHIVE_SHA}"

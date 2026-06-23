#!/usr/bin/env bash
#
# Builds a custom GCE boot image from a COS base with runtime Docker images
# pre-cached in the Docker layer store. Use the resulting image as
# BUCEPHALUS_GCP_RUNNER_BOOT_IMAGE so worker VMs boot with a warm Docker
# cache and the startup-script's docker pull becomes a no-op digest hit
# instead of a 30s+ cold registry fetch.
#
# Usage:
#   ./build-runner-image.sh \
#     --project my-project \
#     --zone us-central1-a \
#     --image-name buc-dev-runner-cos-v1 \
#     --runtime-image us-central1-docker.pkg.dev/my-project/repo/runtime@sha256:... \
#     --runtime-image us-central1-docker.pkg.dev/my-project/repo/another@sha256:...
#
# Multiple --runtime-image flags are pulled in parallel on the build VM.
# The script also pulls the worker image if --worker-image is provided.
#
# After the build completes, set in your pool controller environment:
#   BUCEPHALUS_GCP_RUNNER_BOOT_IMAGE=projects/my-project/global/images/buc-dev-runner-cos-v1

set -euo pipefail

PROJECT_ID=""
ZONE=""
REGION=""
IMAGE_NAME=""
BASE_IMAGE="projects/cos-cloud/global/images/family/cos-stable"
MACHINE_TYPE="e2-standard-2"
DISK_SIZE_GB=100
SERVICE_ACCOUNT=""
NETWORK_TAG=""
SUBNET=""
WORKER_IMAGE=""
RUNTIME_IMAGES=()
BUILD_VM_NAME=""
CLEANUP_VM=true

usage() {
  cat <<'USAGE'
Usage: build-runner-image.sh [options]

Required:
  --project PROJECT_ID        GCP project ID
  --zone ZONE                 GCP zone for the build VM
  --image-name IMAGE_NAME     Name for the output custom image

Optional:
  --region REGION             GCP region (defaults from zone if omitted)
  --base-image IMAGE          Base image family/path (default: cos-stable)
  --machine-type TYPE         Build VM machine type (default: e2-standard-2)
  --disk-size-gb GB           Boot disk size (default: 100)
  --service-account EMAIL     Service account for the build VM
  --subnet SUBNET             Subnetwork for the build VM
  --network-tag TAG           Network tag for the build VM
  --worker-image REF          Worker image ref to also pre-cache
  --runtime-image REF         Runtime image ref to pre-cache (repeatable)
  --no-cleanup                Keep the build VM after image creation
USAGE
  exit 1
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --project) PROJECT_ID="$2"; shift 2 ;;
    --zone) ZONE="$2"; shift 2 ;;
    --region) REGION="$2"; shift 2 ;;
    --image-name) IMAGE_NAME="$2"; shift 2 ;;
    --base-image) BASE_IMAGE="$2"; shift 2 ;;
    --machine-type) MACHINE_TYPE="$2"; shift 2 ;;
    --disk-size-gb) DISK_SIZE_GB="$2"; shift 2 ;;
    --service-account) SERVICE_ACCOUNT="$2"; shift 2 ;;
    --subnet) SUBNET="$2"; shift 2 ;;
    --network-tag) NETWORK_TAG="$2"; shift 2 ;;
    --worker-image) WORKER_IMAGE="$2"; shift 2 ;;
    --runtime-image) RUNTIME_IMAGES+=("$2"); shift 2 ;;
    --no-cleanup) CLEANUP_VM=false; shift ;;
    -h|--help) usage ;;
    *) echo "unknown option: $1" >&2; usage ;;
  esac
done

[[ -n "$PROJECT_ID" ]] || { echo "--project is required" >&2; usage; }
[[ -n "$ZONE" ]] || { echo "--zone is required" >&2; usage; }
[[ -n "$IMAGE_NAME" ]] || { echo "--image-name is required" >&2; usage; }

if [[ -z "$REGION" ]]; then
  REGION="${ZONE%-*}"
fi

BUILD_VM_NAME="buc-image-build-${IMAGE_NAME}-$(date +%s | tail -c 5)"

echo "Building custom runner image: $IMAGE_NAME"
echo "  project:   $PROJECT_ID"
echo "  zone:      $ZONE"
echo "  base:      $BASE_IMAGE"
echo "  runtime images: ${RUNTIME_IMAGES[*]:-none}"
[[ -n "$WORKER_IMAGE" ]] && echo "  worker image: $WORKER_IMAGE"

# --- Create build VM ---

VM_ARGS=(
  --project "$PROJECT_ID"
  --zone "$ZONE"
  --machine-type "$MACHINE_TYPE"
  --boot-disk-size "${DISK_SIZE_GB}GB"
  --boot-disk-type pd-balanced
  --image "$BASE_IMAGE"
  --no-shielded-secure-boot
  --no-shielded-vtpm
  --no-shielded-integrity-monitoring
)

if [[ -n "$SERVICE_ACCOUNT" ]]; then
  VM_ARGS+=("--service-account" "$SERVICE_ACCOUNT" "--scopes" "https://www.googleapis.com/auth/cloud-platform")
fi

if [[ -n "$SUBNET" ]]; then
  VM_ARGS+=("--subnet" "$SUBNET")
fi

if [[ -n "$NETWORK_TAG" ]]; then
  VM_ARGS+=("--tags" "$NETWORK_TAG")
fi

echo "Creating build VM: $BUILD_VM_NAME"
gcloud compute instances create "$BUILD_VM_NAME" "${VM_ARGS[@]}"

cleanup_vm() {
  if $CLEANUP_VM; then
    echo "Deleting build VM: $BUILD_VM_NAME"
    gcloud compute instances delete "$BUILD_VM_NAME" \
      --project "$PROJECT_ID" --zone "$ZONE" --quiet || true
  fi
}
trap cleanup_vm EXIT

echo "Waiting for build VM to be ready..."
sleep 15

# --- Pull images on the build VM ---

PULL_CMDS=()
for ref in "${RUNTIME_IMAGES[@]}"; do
  PULL_CMDS+=("docker pull $ref &")
done
if [[ -n "$WORKER_IMAGE" ]]; then
  PULL_CMDS+=("docker pull $WORKER_IMAGE &")
fi

if [[ ${#PULL_CMDS[@]} -gt 0 ]]; then
  echo "Pulling ${#PULL_CMDS[@]} image(s) in parallel on the build VM..."

  # COS images have Docker pre-installed. Start it, authenticate to Artifact
  # Registry via the metadata token, then pull all images concurrently.
  REMOTE_SCRIPT=$(cat <<'BASH'
set -euo pipefail
systemctl start docker || true
export DOCKER_CONFIG=/tmp/docker-config
mkdir -p "$DOCKER_CONFIG"
TOKEN=$(curl -fsS -H "Metadata-Flavor: Google" \
  "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token" \
  | python3 -c "import sys,json; print(json.load(sys.stdin)['access_token'])")
REGISTRY_HOST=$(echo "$1" | cut -d/ -f1)
echo "{\"auths\":{\"$REGISTRY_HOST\":{\"auth\":\"$(echo -n "oauth2accesstoken:$TOKEN" | base64 -w0)\"}}}" \
  > "$DOCKER_CONFIG/config.json"
BASH
  )

  # Pass the first runtime image ref (or worker image) for registry host detection
  FIRST_REF="${RUNTIME_IMAGES[0]:-$WORKER_IMAGE}"
  gcloud compute ssh "$BUILD_VM_NAME" \
    --project "$PROJECT_ID" --zone "$ZONE" \
    --command "$(printf '%s\n' "$REMOTE_SCRIPT" | sed "s/\\$1/$FIRST_REF/") ${PULL_CMDS[*]} wait"
fi

# --- Stop VM and create image ---

echo "Stopping build VM..."
gcloud compute instances stop "$BUILD_VM_NAME" \
  --project "$PROJECT_ID" --zone "$ZONE" --quiet

echo "Creating custom image: $IMAGE_NAME"
gcloud compute images create "$IMAGE_NAME" \
  --project "$PROJECT_ID" \
  --source-disk "$BUILD_VM_NAME" \
  --source-disk-zone "$ZONE" \
  --storage-location "$REGION" \
  --family bucephalus-runner-cos

IMAGE_PATH="projects/$PROJECT_ID/global/images/$IMAGE_NAME"
echo ""
echo "Custom runner image built successfully:"
echo "  $IMAGE_PATH"
echo ""
echo "Set this as BUCEPHALUS_GCP_RUNNER_BOOT_IMAGE in your pool controller environment."
echo "  BUCEPHALUS_GCP_RUNNER_BOOT_IMAGE=$IMAGE_PATH"

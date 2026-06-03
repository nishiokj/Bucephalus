#!/usr/bin/env bash
# Re-export the 8 canonical Peter Gregory workspaces from the synth-data-pipeline-agents
# repo into this cookbook so the experiment package always carries the latest composer
# output. Run after editing any preset module or scenario YAML upstream.
set -euo pipefail

SRC="${PG_SRC:-${HOME}/Desktop/synth-data-pipeline-agents}"
DEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORKSPACES_DIR="${DEST_DIR}/workspaces"

if [[ ! -d "${SRC}" ]]; then
  echo "synth-data-pipeline-agents not found at ${SRC} (override with PG_SRC=...)" >&2
  exit 1
fi

scenarios=(
  castor_canonical
  customer_of_customer
  regulatory_cascade
  brand_exposure_tweet
  noise_only_day
  near_miss_material
  unrelated_industry_earnings
  out_of_scope_regulation
)

# Recompose from source, then copy.
cd "${SRC}"
rm -rf data/peter_gregory
for s in "${scenarios[@]}"; do
  ./.venv/bin/python -m presets compose "presets/scenarios/${s}.yaml" -o "data/peter_gregory/${s}" >/dev/null
done

rm -rf "${WORKSPACES_DIR}"
mkdir -p "${WORKSPACES_DIR}"
cp -R "${SRC}/data/peter_gregory/"* "${WORKSPACES_DIR}/"

echo "synced ${#scenarios[@]} scenarios into ${WORKSPACES_DIR}"

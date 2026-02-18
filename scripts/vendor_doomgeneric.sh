#!/usr/bin/env bash
set -euo pipefail

# scripts/vendor_doomgeneric.sh: vendor DoomGeneric sources in-tree for ArrOSt builds.
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST_DIR="${ROOT_DIR}/user/doom/third_party/doomgeneric"
SRC_URL="https://github.com/ozkl/doomgeneric.git"

if [[ -d "${DEST_DIR}/.git" || -f "${DEST_DIR}/doomgeneric/doomgeneric.c" ]]; then
  echo "DoomGeneric already present at ${DEST_DIR}"
  exit 0
fi

# CI can provide an empty/incomplete checkout path (e.g. gitlink not initialized).
# Replace it so cloning can succeed deterministically.
if [[ -e "${DEST_DIR}" ]]; then
  echo "DoomGeneric checkout at ${DEST_DIR} is incomplete; re-vendoring"
  rm -rf "${DEST_DIR}"
fi

mkdir -p "$(dirname "${DEST_DIR}")"
git clone --depth 1 "${SRC_URL}" "${DEST_DIR}"

echo "Vendored DoomGeneric at ${DEST_DIR}"
echo "Place a WAD (e.g. doom1.wad) at ${ROOT_DIR}/user/doom/wad/doom1.wad"

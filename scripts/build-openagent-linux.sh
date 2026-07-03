#!/usr/bin/env bash
set -euo pipefail

TARGET="${TARGET:-x86_64-unknown-linux-gnu}"
DOCKER_PLATFORM="${DOCKER_PLATFORM:-linux/amd64}"
MODE="${MODE:-docker}"
IMAGE_TAG="${IMAGE_TAG:-openagent-cli:${TARGET}}"
OUT_DIR="${OUT_DIR:-dist/openagent-linux-${TARGET}}"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
mkdir -p "${repo_root}/${OUT_DIR}"

if [[ "${MODE}" == "cargo" ]]; then
  if command -v rustup >/dev/null 2>&1; then
    rustup target add "${TARGET}" >/dev/null
  fi
  cargo build --release -p openagent-cli --bin openagent --target "${TARGET}"
  cp "${repo_root}/target/${TARGET}/release/openagent" "${repo_root}/${OUT_DIR}/openagent"
else
  tmp_out="$(mktemp -d)"
  trap 'rm -rf "${tmp_out}"' EXIT
  docker build \
    --platform "${DOCKER_PLATFORM}" \
    --target artifact \
    --output "type=local,dest=${tmp_out}" \
    -f "${repo_root}/Dockerfile.openagent-cli" \
    -t "${IMAGE_TAG}" \
    "${repo_root}"
  cp "${tmp_out}/out/openagent" "${repo_root}/${OUT_DIR}/openagent"
fi

chmod +x "${repo_root}/${OUT_DIR}/openagent"
echo "${repo_root}/${OUT_DIR}/openagent"

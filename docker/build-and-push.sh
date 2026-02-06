#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

DOCKER_USERNAME="${DOCKER_USERNAME:-mazzelabs}"
REPO_NAME="${REPO_NAME:-mazze-chain}"
DOCKER_REGISTRY="${DOCKER_REGISTRY:-${DOCKER_USERNAME}/${REPO_NAME}}"
BUILDER_NAME="${BUILDER_NAME:-mazze-builder}"

IMAGES=(
  "node:docker/Dockerfile.node"
  "miner:docker/Dockerfile.miner"
)

PLATFORMS="${PLATFORMS:-linux/amd64}"
# Keep compose/runtime defaults aligned: docker-compose.yml expects *-x86-64.
TAG="${TAG:-x86-64}"
# Optional comma-separated extra tags (example: EXTRA_TAGS=latest,2026.02.06)
EXTRA_TAGS="${EXTRA_TAGS:-}"
NO_CACHE="${NO_CACHE:-}"
VCS_REF="${VCS_REF:-$(git rev-parse --short=12 HEAD 2>/dev/null || echo unknown)}"

echo "Using registry: ${DOCKER_REGISTRY}"
echo "Primary tag suffix: ${TAG}"
echo "VCS ref: ${VCS_REF}"
if [[ -n "${EXTRA_TAGS}" ]]; then
  echo "Extra tag suffixes: ${EXTRA_TAGS}"
fi

echo "Ensuring buildx builder exists..."
if ! docker buildx inspect "${BUILDER_NAME}" >/dev/null 2>&1; then
  docker buildx create --name "${BUILDER_NAME}" --driver docker-container --use
fi
docker buildx use "${BUILDER_NAME}" >/dev/null
docker buildx inspect --bootstrap >/dev/null

if ! docker info >/dev/null 2>&1; then
  echo "Docker is not running or accessible" >&2
  exit 1
fi

echo "Logging in to Docker registry..."
if ! docker login >/dev/null 2>&1; then
  docker login
fi

declare -a PUBLISHED_TAGS=()

for image in "${IMAGES[@]}"; do
  name="${image%%:*}"
  file="${image#*:}"

  declare -a tag_suffixes
  tag_suffixes=("${TAG}")
  if [[ -n "${EXTRA_TAGS}" ]]; then
    IFS=',' read -r -a extra_array <<< "${EXTRA_TAGS}"
    for extra in "${extra_array[@]}"; do
      trimmed="$(echo "${extra}" | xargs)"
      if [[ -n "${trimmed}" ]]; then
        tag_suffixes+=("${trimmed}")
      fi
    done
  fi

  declare -a build_tags
  build_tags=()
  for suffix in "${tag_suffixes[@]}"; do
    full_tag="${DOCKER_REGISTRY}:${name}-${suffix}"
    build_tags+=("${full_tag}")
    PUBLISHED_TAGS+=("${full_tag}")
  done

  echo "Building ${name} for platforms: ${PLATFORMS}"
  echo "Publishing tags:"
  for published in "${build_tags[@]}"; do
    echo "  - ${published}"
  done

  declare -a build_cmd
  build_cmd=(
    docker buildx build
    --platform "${PLATFORMS}"
    --file "${file}"
    --build-arg "VCS_REF=${VCS_REF}"
  )
  for published in "${build_tags[@]}"; do
    build_cmd+=(--tag "${published}")
  done
  if [[ -n "${NO_CACHE}" ]]; then
    build_cmd+=(--no-cache --pull)
  else
    build_cmd+=(
      --cache-from "type=registry,ref=${DOCKER_REGISTRY}:${name}-cache"
      --cache-to "type=registry,mode=max,ref=${DOCKER_REGISTRY}:${name}-cache"
    )
  fi
  build_cmd+=(--push .)
  "${build_cmd[@]}"
done

echo "Done. Published tags:"
for published in "${PUBLISHED_TAGS[@]}"; do
  echo " - ${published}"
done

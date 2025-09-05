#!/bin/bash

set -euo pipefail

DOCKER_USERNAME="mazzelabs"
REPO_NAME="mazze-chain"
DOCKER_REGISTRY="${DOCKER_USERNAME}/${REPO_NAME}"

IMAGES=(
  "node:docker/Dockerfile.node"
  "miner:docker/Dockerfile.miner"
)

PLATFORMS=${PLATFORMS:-"linux/amd64"}
TAG=${TAG:-"latest"}
NO_CACHE=${NO_CACHE:-""}

echo "Ensuring buildx builder exists..."
if ! docker buildx inspect mazze-builder >/dev/null 2>&1; then
  docker buildx create --name mazze-builder --driver docker-container --use
fi
docker buildx inspect --bootstrap >/dev/null

echo "Logging in to Docker Hub..."
if ! docker info >/dev/null 2>&1; then
  echo "Docker is not running or accessible" >&2
  exit 1
fi

if ! docker login >/dev/null 2>&1; then
  docker login
fi

for image in "${IMAGES[@]}"; do
  name="${image%%:*}"
  file="${image#*:}"
  echo "Building ${name} image for platforms: ${PLATFORMS}"
  if [ -n "${NO_CACHE}" ]; then
    docker buildx build \
      --no-cache --pull \
      --platform "${PLATFORMS}" \
      --file "${file}" \
      --tag "${DOCKER_REGISTRY}:${name}-${TAG}" \
      --push \
      .
  else
    docker buildx build \
      --platform "${PLATFORMS}" \
      --file "${file}" \
      --tag "${DOCKER_REGISTRY}:${name}-${TAG}" \
      --cache-from type=registry,ref="${DOCKER_REGISTRY}:${name}-cache" \
      --cache-to type=registry,mode=max,ref="${DOCKER_REGISTRY}:${name}-cache" \
      --push \
      .
  fi
done

echo "Done. Published tags:"
for image in "${IMAGES[@]}"; do
  name="${image%%:*}"
  echo " - ${DOCKER_REGISTRY}:${name}-${TAG}"
done

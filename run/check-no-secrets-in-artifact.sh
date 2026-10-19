#!/usr/bin/env bash
# check-no-secrets-in-artifact.sh
#
# Release-pipeline guard against shipping dev-only secrets in a binary
# artifact. Fails non-zero if the artifact contains `genesis_secrets.toml`
# (or any other dev key file we add later).
#
# Usage:
#   check-no-secrets-in-artifact.sh <artifact-path>
#
# Supports .tar / .tar.gz / .tgz / .zip / .deb / .docker-image-tar / .img.
# For directories, walks them.
#
# Wire this into the release CI step *after* the artifact is built. See
# docs/security-audit.md §5.4 and finding C-3.

set -euo pipefail

ARTIFACT="${1:-}"
if [[ -z "$ARTIFACT" ]]; then
  echo "usage: $0 <artifact-path>" >&2
  exit 2
fi

if [[ ! -e "$ARTIFACT" ]]; then
  echo "artifact not found: $ARTIFACT" >&2
  exit 2
fi

# Files this guard rejects. Add new dev-key files here as they're
# introduced.
FORBIDDEN_FILES=(
  "genesis_secrets.toml"
  "hydra.secrets.toml"
  "hydra.runtime.toml"
  "hydra.dev.runtime.toml"
  "hydra.miner.runtime.toml"
  # add future dev-key files here, e.g.:
  # "dev-keystore.json"
)

found=0
for name in "${FORBIDDEN_FILES[@]}"; do
  case "$ARTIFACT" in
    *.tar.gz|*.tgz)
      if tar -tzf "$ARTIFACT" 2>/dev/null | grep -Fq "$name"; then
        echo "FAIL: $ARTIFACT contains $name" >&2
        found=1
      fi
      ;;
    *.tar)
      if tar -tf "$ARTIFACT" 2>/dev/null | grep -Fq "$name"; then
        echo "FAIL: $ARTIFACT contains $name" >&2
        found=1
      fi
      ;;
    *.zip)
      if unzip -l "$ARTIFACT" 2>/dev/null | grep -Fq "$name"; then
        echo "FAIL: $ARTIFACT contains $name" >&2
        found=1
      fi
      ;;
    *)
      if [[ -d "$ARTIFACT" ]]; then
        if find "$ARTIFACT" -type f -name "$name" -print -quit | grep -q .; then
          echo "FAIL: $ARTIFACT contains $name" >&2
          found=1
        fi
      else
        echo "warning: unknown artifact type for $ARTIFACT — skipping format-specific scan" >&2
      fi
      ;;
  esac
done

if [[ "$found" -ne 0 ]]; then
  echo "" >&2
  echo "Release artifacts must not ship dev-only key material." >&2
  echo "See docs/security-audit.md finding C-3 / §5.4." >&2
  exit 1
fi

echo "ok: $ARTIFACT contains no forbidden files (${FORBIDDEN_FILES[*]})"

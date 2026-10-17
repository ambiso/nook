#!/usr/bin/env bash
# Run from the repository root inside the dev sandbox.
set -euo pipefail
exec nix build --print-build-logs .#checks.x86_64-linux.integration "$@"

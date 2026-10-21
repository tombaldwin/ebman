#!/usr/bin/env bash
# Use the repo's tracked hooks (`.githooks/`): fmt, clippy and the
# protected-name checks on every commit. A fresh clone has none until
# this runs, since git never installs hooks from a checkout.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
git config core.hooksPath .githooks
echo "hooks: core.hooksPath = .githooks ($(ls .githooks | tr '\n' ' '))"

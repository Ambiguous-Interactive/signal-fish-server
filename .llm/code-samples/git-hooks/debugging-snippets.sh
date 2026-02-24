#!/usr/bin/env bash

# Enable debug mode with: DEBUG=1 git commit
if [ "${DEBUG:-0}" = "1" ]; then
  set -x
fi
set -euo pipefail

# Hook not running:
git config core.hooksPath              # Should output: .githooks
git config core.hooksPath .githooks    # Re-enable if needed

# Permission denied:
chmod +x .githooks/pre-commit
git update-index --chmod=+x .githooks/pre-commit

# Command not found:
export PATH="$HOME/.cargo/bin:/usr/local/bin:$PATH"

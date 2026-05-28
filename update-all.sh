#!/usr/bin/env bash
# update-all.sh — refresh system tools, runtimes, and applications
# Logs to ~/scripts/update-all.log

set -uo pipefail

LOG="$HOME/scripts/update-all.log"
echo "===== Update run: $(date) =====" >> "$LOG"

run() {
  echo ">>> $*" >> "$LOG"
  "$@" >> "$LOG" 2>&1 || echo "!!! '$*' exited non-zero (continuing)" >> "$LOG"
}

# -------------------------------------------------------
# Homebrew — CLI tools, Python, Go, and GUI applications
# -------------------------------------------------------
if command -v brew >/dev/null 2>&1; then
  run brew update
  run brew upgrade
  run brew upgrade --cask
  run brew cleanup
fi

# -------------------------------------------------------
# Mac App Store
# -------------------------------------------------------
if command -v mas >/dev/null 2>&1; then
  run mas upgrade
fi

# -------------------------------------------------------
# nvm — install latest LTS Node, carry global packages over
# -------------------------------------------------------
export NVM_DIR="$HOME/.nvm"
if [ -s "$NVM_DIR/nvm.sh" ]; then
  # shellcheck disable=SC1091
  \. "$NVM_DIR/nvm.sh"
  run nvm install --lts --reinstall-packages-from=current --latest-npm
fi

# -------------------------------------------------------
# npm — update global packages on the active Node version
# -------------------------------------------------------
if command -v npm >/dev/null 2>&1; then
  run npm update -g
fi

# -------------------------------------------------------
# pnpm — update pnpm itself and its global packages
# -------------------------------------------------------
if command -v pnpm >/dev/null 2>&1; then
  run pnpm self-update
  run pnpm update -g
fi

echo "===== Done: $(date) =====" >> "$LOG"
echo "" >> "$LOG"

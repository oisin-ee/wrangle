#!/bin/sh
# Finds the wrangle binary for a Herdr plugin command. Herdr runs plugin commands with the
# server's environment; on a host where mise shims are not on that PATH, go through mise.
# Usage: . plugin/wrangle.sh; run_wrangle <args...>
run_wrangle() {
  if command -v wrangle >/dev/null 2>&1; then
    exec wrangle "$@"
  fi
  if command -v mise >/dev/null 2>&1; then
    exec mise x -- wrangle "$@"
  fi
  if [ -x "$HOME/.local/bin/mise" ]; then
    exec "$HOME/.local/bin/mise" x -- wrangle "$@"
  fi
  echo "wrangle: binary not found on PATH or through mise" >&2
  exit 1
}

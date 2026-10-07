#!/bin/sh
# pane.closed / pane.exited hook: drop the reservation attached to the pane, if any.
set -eu
pane="${HERDR_PANE_ID:-}"
if [ -z "$pane" ]; then
  exit 0
fi
. "$(dirname "$0")/wrangle.sh"
run_wrangle release --pane "$pane" --json

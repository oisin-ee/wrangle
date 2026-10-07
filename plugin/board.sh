#!/bin/sh
# Popup board: every host, headroom, reservations, and the queue.
set -eu
. "$(dirname "$0")/wrangle.sh"
run_wrangle status --watch

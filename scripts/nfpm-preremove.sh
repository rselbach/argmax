#!/bin/sh
# Package pre-removal hook: argmax manages per-user shell integration that
# a system package manager cannot remove safely.
echo "argmax: if you added shell integration, run 'argmax uninstall' (per user)" >&2
echo "argmax: before or after removing this package to clean up shell hooks." >&2
exit 0

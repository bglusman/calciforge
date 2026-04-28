#!/bin/sh
# Minimal echo-style wrapper template for any CLI that reads prompt text from
# stdin and writes final answer text to stdout.
#
# Copy this file, set CALCIFORGE_EXEC_BINARY, add any static CLI args after
# this script in Calciforge config, then validate the concrete CLI behavior and
# vendor/subscription terms for your environment.

set -eu

exec "${CALCIFORGE_EXEC_BINARY:?set CALCIFORGE_EXEC_BINARY}" "$@"

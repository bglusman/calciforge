#!/bin/sh
# Minimal echo-style wrapper template for any CLI that reads prompt text from
# stdin and writes final answer text to stdout.
#
# Copy this file, replace the final exec line, then validate the concrete CLI
# behavior and vendor/subscription terms for your environment.

set -eu

prompt="$(cat)"

exec "${CALCIFORGE_EXEC_BINARY:?set CALCIFORGE_EXEC_BINARY}" "$prompt"

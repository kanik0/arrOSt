#!/bin/sh
# scripts/ar-wrapper.sh: filter known benign Darwin ar/ranlib empty-TOC warnings.
set -eu

if [ -n "${ARROST_REAL_AR:-}" ]; then
    real_ar="$ARROST_REAL_AR"
else
    real_ar="$(command -v ar || true)"
fi

if [ -z "$real_ar" ]; then
    echo "ar-wrapper: unable to locate 'ar'" >&2
    exit 127
fi

tmp="$(mktemp "${TMPDIR:-/tmp}/ar-wrapper.XXXXXX.stderr")"
cleanup() {
    rm -f "$tmp"
}
trap cleanup EXIT INT TERM

if ! "$real_ar" "$@" 2>"$tmp"; then
    cat "$tmp" >&2
    exit 1
fi

if [ -s "$tmp" ]; then
    grep -E -v "table of contents is empty \\(no object file members in the library define global symbols\\)" "$tmp" >&2 || true
fi

#!/bin/sh
set -eu

cd "$(dirname "$0")/.."

manifest="docs/qa/screenshots/gate-f-final/SHA256SUMS"
expected_width=1078
expected_height=768

shasum -a 256 -c "$manifest"

if command -v sips >/dev/null 2>&1; then
    while read -r _ path; do
        width=$(sips -g pixelWidth "$path" 2>/dev/null | awk '/pixelWidth/ { print $2 }')
        height=$(sips -g pixelHeight "$path" 2>/dev/null | awk '/pixelHeight/ { print $2 }')
        if [ "$width" != "$expected_width" ] || [ "$height" != "$expected_height" ]; then
            echo "$path: expected ${expected_width}x${expected_height}, got ${width}x${height}" >&2
            exit 1
        fi
    done < "$manifest"
fi

if rg -n 'TODO.*screenshot|screenshot.*TODO' README.md docs/qa docs/UX_UI_QA_REPORT.md; then
    echo "Unresolved screenshot TODO found" >&2
    exit 1
fi

echo "Gate F screenshot baselines verified"

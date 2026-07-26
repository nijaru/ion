#!/usr/bin/env bash

set -euo pipefail

usage() {
	cat <<'EOF'
Usage: scripts/go-format.sh [--check]

Format tracked Go files with golines using gofumpt as the base formatter.
With --check, print the proposed diff and exit non-zero when formatting is
needed.
EOF
}

check_only=0
case "${1:-}" in
"") ;;
--check)
	check_only=1
	;;
--help|-h)
	usage
	exit 0
	;;
*)
	usage >&2
	exit 2
	;;
esac

command -v golines >/dev/null 2>&1 || {
	echo "golines is required on PATH" >&2
	exit 1
}
command -v gofumpt >/dev/null 2>&1 || {
	echo "gofumpt is required on PATH" >&2
	exit 1
}

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

max_len="${ION_GO_MAX_LEN:-120}"
if [[ ! "$max_len" =~ ^[0-9]+$ ]] || ((max_len == 0)); then
	echo "ION_GO_MAX_LEN must be a positive integer" >&2
	exit 2
fi

files=()
while IFS= read -r -d '' file; do
	files+=("$file")
done < <(git ls-files -z -- '*.go')

if ((${#files[@]} == 0)); then
	exit 0
fi

format_args=(
	--base-formatter gofumpt
	--max-len "$max_len"
)

if ((check_only)); then
	formatted="$(golines "${format_args[@]}" --dry-run "${files[@]}")"
	if [[ -n "$formatted" ]]; then
		printf '%s\n' "$formatted"
		echo "Go formatting is not canonical; run scripts/go-format.sh" >&2
		exit 1
	fi
	exit 0
fi

golines "${format_args[@]}" --write-output "${files[@]}"

#!/usr/bin/env bash
# Builds the site and exports it as static files for GitHub Pages (https://cadrs.net).
#
#   site/export.sh [OUT_DIR] [BASE_PATH]     defaults: site/dist, the domain root
#
# Needs the `proa` CLI from our Proa fork (static export is not upstream yet) and the
# Tailwind CLI v4 on PATH:
#   cargo install --git https://github.com/rvdende/proa --branch feat/static-export --locked proa-cli
set -euo pipefail
cd "$(dirname "$0")"
out="${1:-dist}"
base_path="${2-}"

# The cadrs repo's .cargo/config.toml passes nightly-only rustflags (and mold) for the app;
# RUSTFLAGS replaces them for this stable-toolchain build.
export RUSTFLAGS=""

# From the GitHub API: the latest release, which the download links point at, and the star
# count the GitHub button shows until the page fetches a fresh one.
auth=()
[[ -n "${GITHUB_TOKEN:-}" ]] && auth=(-H "Authorization: Bearer $GITHUB_TOKEN")
api() { curl -fsSL "${auth[@]}" "https://api.github.com/repos/rvdende/cadrs$1" || true; }
if [[ -z "${CADRS_RELEASE:-}" ]]; then
    CADRS_RELEASE="$(api /releases/latest | sed -n 's/^  "tag_name": "\(.*\)",$/\1/p')"
fi
if [[ -z "${CADRS_STARS:-}" ]]; then
    CADRS_STARS="$(api "" | sed -n 's/^  "stargazers_count": \([0-9]*\),$/\1/p')"
fi
export CADRS_RELEASE="${CADRS_RELEASE:-}" CADRS_STARS="${CADRS_STARS:-}"
echo "release: ${CADRS_RELEASE:-unknown (links go to the releases page)}"
echo "stars: ${CADRS_STARS:-unknown (the page fetches them)}"

proa build --locked --artifact-out target/site/cadrs-site
proa export --bin target/site/cadrs-site --out "$out" --base-path "$base_path"

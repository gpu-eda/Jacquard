#!/usr/bin/env bash
# Ephemeral-runner entrypoint: mint a registration token from a PAT, register
# as a single-use ephemeral runner, run one job, then deregister on exit.
#
# Required env (passed by blackwell-runner.service):
#   RUNNER_PAT     fine-grained PAT with repo Administration: read+write
#   GH_OWNER       e.g. gpu-eda
#   GH_REPO        e.g. Jacquard
# Optional:
#   RUNNER_LABELS  default: self-hosted,cuda,blackwell,sm_120
#   RUNNER_NAME    default: blackwell-<short-random>
set -euo pipefail

: "${RUNNER_PAT:?RUNNER_PAT is required}"
: "${GH_OWNER:?GH_OWNER is required}"
: "${GH_REPO:?GH_REPO is required}"
LABELS="${RUNNER_LABELS:-self-hosted,cuda,blackwell,sm_120}"
NAME="${RUNNER_NAME:-blackwell-$(tr -dc a-z0-9 </dev/urandom | head -c 8)}"

api() {
    curl -fsSL -X POST \
        -H "Authorization: Bearer ${RUNNER_PAT}" \
        -H "Accept: application/vnd.github+json" \
        -H "X-GitHub-Api-Version: 2022-11-28" \
        "https://api.github.com/repos/${GH_OWNER}/${GH_REPO}/actions/runners/${1}"
}

echo "Requesting registration token for ${GH_OWNER}/${GH_REPO}…"
REG_TOKEN="$(api registration-token | jq -r .token)"
[ -n "${REG_TOKEN}" ] && [ "${REG_TOKEN}" != "null" ] || { echo "failed to obtain registration token" >&2; exit 1; }

cleanup() {
    echo "Removing runner registration…"
    REMOVE_TOKEN="$(api remove-token | jq -r .token || true)"
    [ -n "${REMOVE_TOKEN}" ] && [ "${REMOVE_TOKEN}" != "null" ] \
        && ./config.sh remove --token "${REMOVE_TOKEN}" || true
}
trap cleanup EXIT

./config.sh \
    --unattended \
    --ephemeral \
    --url "https://github.com/${GH_OWNER}/${GH_REPO}" \
    --token "${REG_TOKEN}" \
    --name "${NAME}" \
    --labels "${LABELS}" \
    --replace

# --ephemeral: run.sh exits after exactly one job; the systemd unit then
# re-spawns a fresh container for the next.
./run.sh

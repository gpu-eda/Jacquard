#!/usr/bin/env bash
# Periodic Docker hygiene for the ephemeral GitHub Actions runners.
# Gentle/filtered: only removes resources unused for >7d (containers >1d),
# preserving hot build cache so CI stays fast. Safe to run during a job:
# never removes in-use images or fresh cache.
#
# Images: dangling (untagged) only, never `-a`. The runner images are built
# locally and run with `--pull never`, so deleting one stops its runner until
# someone rebuilds it. `prune -a` with `until=` judges an image by its *creation*
# time, so a months-old runner image is eligible whenever no container uses it,
# which is the state at boot, when docker-prune.timer's Persistent=true catch-up
# run fires. That is how both runner images were lost on 2026-10-20. Do not try
# to protect them with `--filter label!=...` instead: on this Docker (29.x,
# containerd image store) that filter either removed labelled images or matched
# nothing, depending on its form. Runner jobs get no Docker socket, so nothing
# else creates tagged images on this host.
#
# Install: sudo install -m 755 docker-prune.sh /usr/local/bin/docker-prune.sh
set -euo pipefail
echo "[docker-prune] $(date -Is) starting"
docker container prune -f --filter "until=24h"
docker image     prune -f  --filter "until=168h"
docker builder   prune -f  --filter "until=168h"
echo "[docker-prune] $(date -Is) done. Usage now:"
docker system df

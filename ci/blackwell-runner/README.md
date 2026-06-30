# Blackwell CI runner

A self-hosted GitHub Actions runner that gives CI a real **NVIDIA Blackwell**
GPU (compute capability `sm_120`), which GitHub-hosted runners don't yet offer.
It runs the `cuda-blackwell` job in `.github/workflows/ci.yml` — building the
CUDA backend with `JACQUARD_CUDA_ARCH=native` and running a `--check-with-cpu`
simulation on the actual hardware.

Host today: `nvidia1.local` — RTX 5060 Ti (sm_120, 16 GB), CUDA 12.8, driver
580, Docker 29.x with the `nvidia` container runtime preconfigured.

## Why ephemeral + containerized

`gpu-eda/Jacquard` is a **public** repo. A self-hosted runner that executes
untrusted pull-request code on a host on your network is dangerous. Two layers
contain that:

1. **Ephemeral container per job.** Each job runs in a throwaway Docker
   container (`--ephemeral`, `--rm`) with only the GPU and the checkout. It is
   destroyed when the job ends; nothing persists to the host.
2. **Fork-PR gating** in the workflow: the job runs on pushes to `main`, on
   same-repo branch PRs, and on fork PRs **only** once a maintainer adds the
   `ci:blackwell` label after reviewing the diff. Untrusted forks never run
   automatically.

These are defence-in-depth: keep both.

## One-time host setup

Prerequisites (already true on `nvidia1.local`): Docker + `nvidia-container-toolkit`,
verified with `docker run --rm --gpus all nvidia/cuda:12.8.1-base-ubuntu24.04 nvidia-smi`.

1. **Create a PAT.** Fine-grained, resource owner `gpu-eda`, repository
   `Jacquard` only, **Repository permissions → Administration: Read and write**
   (required to mint runner registration tokens). Nothing else. Short expiry +
   calendar reminder to rotate.

2. **Build the image** (from a repo checkout on the host):
   ```sh
   docker build -t blackwell-runner:latest ci/blackwell-runner
   ```

3. **Install the service + secret:**
   ```sh
   sudo install -d -m 700 /etc/blackwell-runner
   sudo tee /etc/blackwell-runner/env >/dev/null <<'EOF'
   RUNNER_PAT=github_pat_REPLACE_ME
   GH_OWNER=gpu-eda
   GH_REPO=Jacquard
   EOF
   sudo chmod 600 /etc/blackwell-runner/env
   sudo cp ci/blackwell-runner/blackwell-runner.service /etc/systemd/system/
   sudo systemctl daemon-reload
   sudo systemctl enable --now blackwell-runner
   ```

4. **Verify** the runner appears online with labels `cuda, blackwell, sm_120`
   under the repo's Settings → Actions → Runners, or:
   ```sh
   gh api repos/gpu-eda/Jacquard/actions/runners -q '.runners[].name'
   ```

## Operating

- **Logs:** `journalctl -u blackwell-runner -f` (host) and the job log on GitHub.
- **Rebuild after image changes:** `docker build -t blackwell-runner:latest
  ci/blackwell-runner && sudo systemctl restart blackwell-runner`.
- **Pause:** `sudo systemctl stop blackwell-runner` (in-flight job finishes; no
  new one starts).
- **Rotate the PAT:** edit `/etc/blackwell-runner/env`, `systemctl restart`.

## Performance note

Ephemeral containers carry no cargo/`target` cache, so each run compiles from
scratch (~a few minutes). That is the cost of isolation. If it becomes a
bottleneck, bake a warmed cargo registry into the image or mount a **read-only**
cache — do not mount a writable host cache that untrusted PR code could poison.

## Migration path

For a multi-node GPU fleet, the natural next step is k3s + the NVIDIA GPU
operator + Actions Runner Controller (ARC) with ephemeral runner pods. The
workflow's `runs-on: [self-hosted, cuda, blackwell]` is unchanged by that move.

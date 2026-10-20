# AMD ROCm CI runner

A self-hosted GitHub Actions runner on the **Raphael iGPU** (gfx1036, RDNA2)
integrated into `nvidia1.local`'s Ryzen 5 7600. It runs the `HIP Tests (ROCm
backend)` job (`runs-on: [self-hosted, amd, rocm]`). Why an APU, and why ROCm
reports it as gfx1030, is recorded in
[the AMD laptop backend spike](../../docs/architecture/decisions/spikes/amd-laptop-backend.md).

The packaging mirrors [`../blackwell-runner/`](../blackwell-runner/README.md):
one ephemeral container per job, registered org-level from a PAT. Read that
README for the security model and the PAT permissions; everything there applies
here.

## Images

Two layers, both built on the runner host:

- `Dockerfile` builds **`amd-bench`**: ROCm 7.2.4 (digest-pinned), Mesa RADV
  Vulkan, Rust, and `HSA_OVERRIDE_GFX_VERSION=10.3.0`.
- `Dockerfile.runner` builds **`amd-runner`** on top: the Actions runner and
  `entrypoint.sh`.

```sh
docker build -t amd-bench:latest ci/amd-runner
docker build -f ci/amd-runner/Dockerfile.runner -t amd-runner:latest ci/amd-runner
sudo systemctl restart amd-runner
```

These sources were recovered on 2026-10-20 from BuildKit build history after
the images were pruned. They had never been committed.

## Service

[`amd-runner.service`](amd-runner.service) is installed at
`/etc/systemd/system/amd-runner.service`, with the PAT
in `/etc/amd-runner/env` (same format as the Blackwell runner's). It passes the
GPU through with `--device /dev/kfd`, the DRI render and card nodes, and their
numeric groups, and runs with `seccomp=unconfined`, which ROCm needs.

## Host hygiene

[`../runner-host/`](../runner-host/) holds the weekly Docker prune that runs on
the same host. It prunes dangling images only, so it can never delete the
runner images: see the comment in `docker-prune.sh`.

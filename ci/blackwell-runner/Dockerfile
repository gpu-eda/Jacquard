# Ephemeral GitHub Actions runner image for the Blackwell CI box.
#
# Bakes the CUDA 12.8 toolkit + Rust + the Actions runner so a job needs no
# per-run toolchain install. Run one container per job with `--gpus all` and
# `--ephemeral` (see ../blackwell-runner.service); the container processes a
# single job in a fresh sandbox and exits, so untrusted PR code never persists
# on the host.
#
# Build (on the runner host):
#   docker build -t blackwell-runner:latest ci/blackwell-runner
FROM nvidia/cuda:12.8.1-devel-ubuntu24.04

ARG RUNNER_VERSION=2.335.1
ENV DEBIAN_FRONTEND=noninteractive

# Build + runner runtime deps. clang/llvm cover the cc-rs C++ paths
# (spiflash_model); jq parses the registration-token API response; python3
# runs the CI test-suite validators (stdlib only, no pip).
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl git jq sudo python3 \
        build-essential pkg-config libssl-dev clang llvm \
    && rm -rf /var/lib/apt/lists/*

# Unprivileged runner user; the GitHub runner refuses to run config.sh as root.
RUN useradd --create-home --shell /bin/bash runner
USER runner
WORKDIR /home/runner

# Rust toolchain (matches the dtolnay/rust-toolchain@stable the workflow would
# otherwise install per job — baked here so the build starts immediately).
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --profile minimal --default-toolchain stable
ENV PATH=/home/runner/.cargo/bin:/usr/local/cuda/bin:$PATH

# Actions runner.
WORKDIR /home/runner/actions-runner
RUN curl -fsSL -o runner.tar.gz \
      "https://github.com/actions/runner/releases/download/v${RUNNER_VERSION}/actions-runner-linux-x64-${RUNNER_VERSION}.tar.gz" \
    && tar xzf runner.tar.gz && rm runner.tar.gz

# installdependencies.sh needs root; run it before dropping back to runner.
USER root
RUN ./bin/installdependencies.sh
USER runner

COPY --chown=runner:runner --chmod=0755 entrypoint.sh /home/runner/entrypoint.sh
ENTRYPOINT ["/home/runner/entrypoint.sh"]

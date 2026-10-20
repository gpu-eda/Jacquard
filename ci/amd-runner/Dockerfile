# amd-bench: one image carrying BOTH compute toolchains for the AMD iGPU
#   - HIP / ROCm      -> near-term production path (built for gfx1030 + HSA override)
#   - Vulkan (RADV)   -> CubeCL / wgpu portable path (no ROCm needed)
# so CubeCL-vs-HIP can be benchmarked head-to-head on the same Raphael iGPU.
#
# Build (on the runner host), then the runner layer on top:
#   docker build -t amd-bench:latest ci/amd-runner
#   docker build -f ci/amd-runner/Dockerfile.runner -t amd-runner:latest ci/amd-runner
#
# The base is pinned by digest to the rocm/dev-ubuntu-24.04 the original image
# was built from (ROCm 7.2.4). CI's hip-rocm build container is version-matched
# to this runtime (.github/workflows/build.yml), so bump both together.
FROM rocm/dev-ubuntu-24.04@sha256:bdc8e61026cbb844ede93d44d2c50055f51ebb2041906b60182bf3bee3139054

ENV DEBIAN_FRONTEND=noninteractive

# Mesa RADV Vulkan driver (the CubeCL/wgpu backend rides this on AMD), plus
# vulkan-tools for vulkaninfo, and Rust build prerequisites.
RUN apt-get update && apt-get install -y --no-install-recommends \
      mesa-vulkan-drivers vulkan-tools libvulkan1 \
      git ca-certificates curl jq build-essential pkg-config \
 && rm -rf /var/lib/apt/lists/*

# Rust toolchain for CubeCL.
ENV RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/opt/cargo \
    PATH=/opt/cargo/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --default-toolchain stable --profile minimal \
 && rustc --version && cargo --version

# Present the Raphael gfx1036 iGPU as gfx1030 so ROCm's prebuilt library
# kernels apply. Build HIP code with: hipcc --offload-arch=gfx1030
ENV HSA_OVERRIDE_GFX_VERSION=10.3.0

WORKDIR /work

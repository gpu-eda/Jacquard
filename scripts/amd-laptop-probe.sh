#!/usr/bin/env bash
# Jacquard — AMD laptop GPU probe
#
# Answers one question: would Jacquard's GPU kernel run on this machine?
#
# You do NOT need to build Jacquard. This compiles a ~40-line HIP program that
# uses exactly what Jacquard's kernel uses and nothing else:
#
#   * warpSize == 32          — Jacquard requires wave32 (RDNA); wave64 is rejected
#   * __shfl_down_sync        — the only architecture-specific intrinsic it uses
#   * __syncthreads           — universal
#
# That's the whole surface. Jacquard simulates AND gates: no rocBLAS, no
# hipBLAS, no matrix/WMMA kernels. Most AMD-laptop grief reported by projects
# like llama.cpp comes from rocBLAS shipping per-architecture tuned libraries
# that omit laptop parts — which is why their experience may not predict ours,
# and why we'd like a real data point.
#
# Requires: ROCm + hipcc (`hipcc --version` should work). Nothing is installed,
# nothing needs root, and nothing is written outside a temp dir.
#
# Usage:  bash amd-laptop-probe.sh
#
# Please paste the whole REPORT block back to us. Thanks for helping!

set -uo pipefail

echo "=============================================="
echo " Jacquard AMD laptop probe"
echo "=============================================="
echo

# ── environment ──────────────────────────────────────────────────────────────
OS="$(uname -srm 2>/dev/null || echo unknown)"
KERNEL="$(uname -r 2>/dev/null || echo unknown)"
DISTRO="$(. /etc/os-release 2>/dev/null && echo "${PRETTY_NAME:-unknown}" || echo unknown)"
CPU="$(grep -m1 'model name' /proc/cpuinfo 2>/dev/null | cut -d: -f2- | sed 's/^ *//' || echo unknown)"

export PATH="/opt/rocm/bin:${PATH}"

HIPCC="$(command -v hipcc || true)"
if [ -z "$HIPCC" ]; then
  echo "hipcc not found on PATH (looked in /opt/rocm/bin too)."
  echo "This probe needs ROCm installed. If you don't have it, that is itself"
  echo "a useful answer — please tell us so and stop here."
  exit 2
fi

ROCM_VER="$(cat /opt/rocm/.info/version 2>/dev/null || echo unknown)"
HIP_VER="$(hipcc --version 2>/dev/null | grep -m1 -i 'HIP version' | sed 's/^ *//' || echo unknown)"
PLATFORM="$(hipconfig --platform 2>/dev/null || echo unknown)"

# ── which GPU, and which gfx target? ─────────────────────────────────────────
GFX="$(rocminfo 2>/dev/null | grep -m1 -oE 'gfx[0-9a-f]+' || true)"
GPU_NAME="$(rocminfo 2>/dev/null | grep -A3 -m1 'Vendor Name: *AMD' | grep -m1 'Marketing Name' | cut -d: -f2- | sed 's/^ *//' || true)"
[ -z "$GPU_NAME" ] && GPU_NAME="$(rocm-smi --showproductname 2>/dev/null | grep -m1 -i 'card series' | cut -d: -f2- | sed 's/^ *//' || true)"
GFX_TARGET_VER="$(grep -h 'gfx_target_version' /sys/class/kfd/kfd/topology/nodes/*/properties 2>/dev/null | awk '$2 != 0 {print $2; exit}' || true)"

# ── the actual test ──────────────────────────────────────────────────────────
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

cat > "$TMP/probe.hip" <<'EOF'
#include <hip/hip_runtime.h>
#include <cstdio>

// Mirrors Jacquard's kernel surface: a __shfl_down_sync reduction across a
// 32-lane wave, plus __syncthreads. Nothing else is architecture-specific.
__global__ void wave_reduce(int *out) {
  __shared__ int scratch[32];
  int lane = threadIdx.x;
  int v = lane + 1;                     // 1..32
  __syncthreads();
  // Butterfly reduction via warp shuffle — sum(1..32) == 528.
  for (int off = 16; off > 0; off >>= 1) {
    v += __shfl_down_sync(0xffffffffu, v, off);
  }
  scratch[lane] = v;
  __syncthreads();
  if (lane == 0) *out = scratch[0];
}

int main() {
  int dev = 0;
  hipDeviceProp_t prop;
  if (hipGetDeviceProperties(&prop, dev) != hipSuccess) {
    printf("PROBE: hipGetDeviceProperties FAILED (no usable GPU?)\n");
    return 2;
  }
  printf("PROBE_GPU_NAME=%s\n", prop.gcnArchName);
  printf("PROBE_WARP_SIZE=%d\n", prop.warpSize);
  printf("PROBE_CUS=%d\n", prop.multiProcessorCount);

  if (prop.warpSize != 32) {
    // Jacquard hard-rejects this: kernel_v1.hip.cpp::validate_warp_size().
    printf("PROBE_VERDICT=UNSUPPORTED_WAVE64\n");
    return 3;
  }

  int *d = nullptr, h = 0;
  if (hipMalloc(&d, sizeof(int)) != hipSuccess) {
    printf("PROBE_VERDICT=HIPMALLOC_FAILED\n");
    return 4;
  }
  hipLaunchKernelGGL(wave_reduce, dim3(1), dim3(32), 0, 0, d);
  hipError_t e = hipDeviceSynchronize();
  if (e != hipSuccess) {
    printf("PROBE_LAUNCH_ERROR=%s\n", hipGetErrorString(e));
    printf("PROBE_VERDICT=KERNEL_LAUNCH_FAILED\n");
    return 5;
  }
  hipMemcpy(&h, d, sizeof(int), hipMemcpyDeviceToHost);
  printf("PROBE_SHFL_RESULT=%d (expect 528)\n", h);
  printf("PROBE_VERDICT=%s\n", (h == 528) ? "OK" : "WRONG_RESULT");
  return (h == 528) ? 0 : 6;
}
EOF

echo "--- compiling (native arch) ---"
COMPILE_LOG="$TMP/compile.log"
if hipcc "$TMP/probe.hip" -o "$TMP/probe" >"$COMPILE_LOG" 2>&1; then
  COMPILE="OK"
else
  COMPILE="FAILED"
fi

RUN_OUT=""
RUN_RC="n/a"
if [ "$COMPILE" = "OK" ]; then
  echo "--- running ---"
  RUN_OUT="$("$TMP/probe" 2>&1)"
  RUN_RC=$?
fi

# Does hipcc emit code for the laptop targets? This is the specific thing
# Jacquard doesn't currently do (its default target list is gfx1030,gfx1100).
echo "--- cross-compiling for each laptop target ---"
XC=""
for arch in gfx1030 gfx1100 gfx1103 gfx1150 gfx1151; do
  if hipcc --offload-arch="$arch" -c "$TMP/probe.hip" -o "$TMP/x.o" >/dev/null 2>&1; then
    XC="${XC}  ${arch}: compiles"$'\n'
  else
    XC="${XC}  ${arch}: FAILS"$'\n'
  fi
done

# ── report ───────────────────────────────────────────────────────────────────
cat <<REPORT

========== REPORT (please paste all of this back) ==========
os                : ${OS}
distro            : ${DISTRO}
kernel            : ${KERNEL}
cpu               : ${CPU}
rocm              : ${ROCM_VER}
hip               : ${HIP_VER}
hip platform      : ${PLATFORM}
gpu (rocminfo)    : ${GPU_NAME:-unknown}
gfx target        : ${GFX:-unknown}
gfx_target_version: ${GFX_TARGET_VER:-unknown}

probe compile     : ${COMPILE}
probe run rc      : ${RUN_RC}
${RUN_OUT}

cross-compile:
${XC}
============================================================

REPORT

if [ "$COMPILE" != "OK" ]; then
  echo "Compile failed. Last lines:"
  tail -15 "$COMPILE_LOG"
  echo
  echo "(Please include the above with the report.)"
fi

case "${RUN_OUT}" in
  *PROBE_VERDICT=OK*)
    echo "Verdict: this GPU does what Jacquard's kernel needs (wave32 + shuffle)."
    echo "That's the good outcome — thank you!"
    ;;
  *UNSUPPORTED_WAVE64*)
    echo "Verdict: wave64 GPU. Jacquard requires wave32 (RDNA) and would refuse."
    ;;
  *)
    echo "Verdict: inconclusive — the report above still tells us what we need."
    ;;
esac

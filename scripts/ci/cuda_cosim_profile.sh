#!/usr/bin/env bash
# CUDA cosim performance profiling harness (issue #122).
#
# Produces a measurement-driven baseline for the CUDA/HIP cosim backend on the
# heavy `mcu_soc` SPI-flash fixture — the same fixture the Stage C correctness
# gate runs (scripts/ci/cosim_cpu_check.sh, COSIM_SCOPE=flash), so timing here
# is directly comparable to the parity run. Three phases:
#
#   1. Repeated-trial timing baseline (no profiler attached) — runs the flash
#      cosim TRIALS times, discards the first as warmup, and aggregates the
#      per-edge "TOTAL (instrumented)" µs/tick the cosim summary already prints.
#      This is the real warm baseline issue #122 asks for (vs. the single-shot
#      CI-byproduct timing the issue was opened from).
#   2. nsys (Nsight Systems) profile WITH unified-memory page-fault tracing —
#      the direct test of the central hypothesis: that cudaMallocManaged page
#      migration is the per-edge tax on the heavy design (16 MiB firmware +
#      persistent FlashState + large state/sram buffers touched every batch).
#      Emits the CUDA kernel/mem-time summaries + the UM CPU/GPU page-fault
#      reports.
#   3. ncu (Nsight Compute) per-kernel detail — OPTIONAL (NCU=1). Kernel replay
#      is slow, so it runs a tiny edge count, and cloud GPU runners frequently
#      restrict the hardware counters it needs (ERR_NVGPUCTRPERM) — hence
#      best-effort, gated, and never the primary deliverable.
#
# This builds nothing — point JACQUARD_BIN at an already-built `--features cuda`
# (or `--features hip`) binary. Designed to run both from CI (cuda-cosim-profile
# workflow) and by hand in an interactive session on a CUDA box / the T4 runner.
#
# Env:
#   JACQUARD_BIN  path to a CUDA/HIP jacquard binary (default target/release/jacquard)
#   EDGES         --max-clock-edges for the baseline + nsys run (default 10000,
#                 matches the Stage C flash gate)
#   TRIALS        repeated-trial count for the baseline (default 5; trial 1 = warmup)
#   NCU           1 to also run the Nsight Compute kernel profile (default 0)
#   NCU_EDGES     --max-clock-edges for the ncu run — keep small, kernel replay
#                 is slow (default 200)
#   OUTDIR        artifact output directory (default ./perf_out)
set -euo pipefail

BIN="${JACQUARD_BIN:-target/release/jacquard}"
EDGES="${EDGES:-10000}"
TRIALS="${TRIALS:-3}"
NCU="${NCU:-0}"
NCU_EDGES="${NCU_EDGES:-200}"
OUTDIR="${OUTDIR:-perf_out}"

if [ ! -x "$BIN" ]; then
    echo "error: jacquard binary not found at '$BIN'" >&2
    echo "       build it first: cargo build -r --features cuda --bin jacquard" >&2
    exit 2
fi

mkdir -p "$OUTDIR"
SUMMARY="$OUTDIR/summary.md"
: > "$SUMMARY"

# The heavy mcu_soc SPI-flash fixture — byte-for-byte the Stage C gate command
# (scripts/ci/cosim_cpu_check.sh, flash scope). Self-contained config so it runs
# on a fresh checkout (committed firmware, no chipflow build / benchmarks submod).
# Spelled once here; the baseline loop and the nsys/ncu wrappers all reuse it.
# --output-vcd is kept on every run on purpose: the Stage C gate (the source of
# issue #122's quoted numbers) writes it too, and "Output VCD write" is one of
# the per-edge categories the cosim summary measures — dropping it would make
# this baseline non-comparable.
COSIM_ARGS=(
    cosim tests/mcu_soc/data/6_final.v
    --config tests/mcu_soc/sim_config_selfcontained.json --top-module top
)
flash_cosim() {
    local edges="$1"; shift
    "$BIN" "${COSIM_ARGS[@]}" \
        --max-clock-edges "$edges" --output-vcd "$OUTDIR/mcu_flash.vcd" "$@"
}

# Pull the per-edge "TOTAL (instrumented)  N.Nμs/tick" value out of a run log.
# The µs glyph is glued to the number (Rust "{:>8.1}μs/tick"), so isolate the
# TOTAL line and take its first float — robust to the multibyte µs char.
per_edge_us() {
    grep -F 'TOTAL (instrumented)' "$1" | grep -oE '[0-9]+\.[0-9]+' | head -1
}
mean_batch() {
    grep -oE 'mean batch=[0-9]+\.[0-9]+' "$1" | grep -oE '[0-9]+\.[0-9]+' | head -1
}

{
    echo "# CUDA cosim profile — mcu_soc flash (#122)"
    echo
    echo "- binary: \`$BIN\`"
    echo "- edges (baseline/nsys): $EDGES · trials: $TRIALS (trial 1 = warmup, discarded)"
    echo
} >> "$SUMMARY"

# ── Phase 1: repeated-trial timing baseline (no profiler) ────────────────────
echo "=== Phase 1: repeated-trial baseline ($TRIALS trials, $EDGES edges) ==="
declare -a samples=()
mb=""
for t in $(seq 1 "$TRIALS"); do
    log="$OUTDIR/baseline_trial_${t}.txt"
    flash_cosim "$EDGES" 2>&1 | tee "$log" >/dev/null
    us="$(per_edge_us "$log" || true)"
    [ -z "$mb" ] && mb="$(mean_batch "$log" || true)"
    echo "  trial $t: ${us:-?} µs/edge"
    if [ "$t" -gt 1 ] && [ -n "$us" ]; then
        samples+=("$us")
    fi
done

printf '## Phase 1 — repeated-trial baseline\n\n' >> "$SUMMARY"
if [ "${#samples[@]}" -gt 0 ]; then
    stats="$(printf '%s\n' "${samples[@]}" | awk '
        NR==1 {min=max=$1}
        {sum+=$1; if($1<min)min=$1; if($1>max)max=$1; n++}
        END {printf "%.2f %.2f %.2f", min, sum/n, max}')"
    read -r bmin bmean bmax <<< "$stats"
    {
        echo "Per-edge \`TOTAL (instrumented)\` µs/edge over ${#samples[@]} warm trials:"
        echo
        echo "| metric | µs/edge |"
        echo "|---|---|"
        echo "| min  | $bmin |"
        echo "| mean | $bmean |"
        echo "| max  | $bmax |"
        echo
        echo "Mean batch: ${mb:-?} (100%-batched flash schedule)."
    } >> "$SUMMARY"
    echo "  baseline: min=$bmin mean=$bmean max=$bmax µs/edge (mean batch=${mb:-?})"
else
    echo "WARNING: no per-edge samples parsed — check the cosim summary format." | tee -a "$SUMMARY"
fi
echo >> "$SUMMARY"

# ── Phase 2: nsys profile with unified-memory page-fault tracing ─────────────
echo "=== Phase 2: nsys profile (UM page-fault tracing, $EDGES edges) ==="
printf '## Phase 2 — nsys (Nsight Systems) + unified-memory migration\n\n' >> "$SUMMARY"
if ! command -v nsys >/dev/null 2>&1; then
    echo "WARNING: nsys not on PATH — skipping the timeline/UM profile." | tee -a "$SUMMARY"
else
    rep="$OUTDIR/flash_nsys"
    # --cuda-um-*-page-faults capture the managed-memory migration traffic that
    # is the #122 hypothesis; osrt+cuda+nvtx give the per-edge kernel-chain
    # timeline (state_prep → apply_flash_din → simulate×N → flash_model_step →
    # gpu_io_step → snapshot).
    nsys profile \
        --trace=cuda,nvtx,osrt \
        --cuda-um-cpu-page-faults=true \
        --cuda-um-gpu-page-faults=true \
        --output "$rep" --force-overwrite true \
        "$BIN" "${COSIM_ARGS[@]}" \
        --max-clock-edges "$EDGES" --output-vcd "$OUTDIR/mcu_flash_nsys.vcd" \
        2>&1 | tee "$OUTDIR/nsys_profile.log" >/dev/null || true

    # Summarise: kernel time, GPU mem-op time, and the two UM page-fault reports.
    stats_txt="$OUTDIR/nsys_stats.txt"
    nsys stats \
        --report cuda_gpu_kern_sum \
        --report cuda_gpu_mem_time_sum \
        --report cuda_um_cpu_page_faults_sum \
        --report cuda_um_gpu_page_faults_sum \
        "$rep.nsys-rep" 2>&1 | tee "$stats_txt" >/dev/null || true

    {
        echo "Report: \`$(basename "$rep").nsys-rep\` (full timeline, uploaded as artifact)."
        echo
        echo "Key \`nsys stats\` tables (kernel time, GPU mem-op time, UM CPU/GPU page faults):"
        echo
        echo '```'
        # Trim to keep the step summary readable; full text in nsys_stats.txt.
        head -120 "$stats_txt" 2>/dev/null || echo "(nsys stats produced no output)"
        echo '```'
    } >> "$SUMMARY"
fi
echo >> "$SUMMARY"

# ── Phase 3 (optional): ncu per-kernel detail ────────────────────────────────
printf '## Phase 3 — ncu (Nsight Compute) per-kernel detail\n\n' >> "$SUMMARY"
if [ "$NCU" != 1 ]; then
    echo "Skipped (set NCU=1 to enable)." >> "$SUMMARY"
    echo "=== Phase 3: ncu SKIPPED (NCU != 1) ==="
elif ! command -v ncu >/dev/null 2>&1; then
    echo "WARNING: ncu not on PATH — skipping kernel-detail profile." | tee -a "$SUMMARY"
else
    echo "=== Phase 3: ncu kernel profile ($NCU_EDGES edges) ==="
    ncu_rep="$OUTDIR/flash_ncu"
    # Kernel replay is slow → tiny edge count. Cloud GPU runners frequently
    # block the HW counters (ERR_NVGPUCTRPERM); this is best-effort.
    ncu --set full --target-processes all \
        --export "$ncu_rep" -f \
        "$BIN" "${COSIM_ARGS[@]}" \
        --max-clock-edges "$NCU_EDGES" --output-vcd "$OUTDIR/mcu_flash_ncu.vcd" \
        2>&1 | tee "$OUTDIR/ncu_profile.log" >/dev/null || true

    {
        if grep -q 'ERR_NVGPUCTRPERM' "$OUTDIR/ncu_profile.log" 2>/dev/null; then
            echo "ncu could not read HW counters on this runner (ERR_NVGPUCTRPERM)."
            echo "The GPU needs profiling enabled for non-root users — set the kernel-module"
            echo "param \`NVreg_RestrictProfilingToAdminUsers=0\` (or run ncu as root)."
        else
            echo "Report: \`$(basename "$ncu_rep").ncu-rep\` (uploaded as artifact)."
            echo
            echo '```'
            ncu --import "$ncu_rep.ncu-rep" --page details 2>/dev/null | head -80 || echo "(ncu import produced no output)"
            echo '```'
        fi
    } >> "$SUMMARY"
fi

echo
echo "=== profile complete — artifacts in $OUTDIR/ ==="
echo "Summary written to $SUMMARY"

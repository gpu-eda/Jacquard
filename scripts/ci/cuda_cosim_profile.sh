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
NCU_EDGES="${NCU_EDGES:-20}"
# ncu replays each profiled kernel launch once per metric pass, so an unbounded
# `--set full` over every kernel × every edge blows the CI job timeout (the first
# attempt hit the 60-min cap). Keep ncu bounded and targeted: `basic` metric set
# (Speed-of-Light + occupancy + launch), filter to the hotspot kernel only, and
# cap the number of profiled launches. All overridable for a deeper manual run.
NCU_SET="${NCU_SET:-basic}"
NCU_KERNEL="${NCU_KERNEL:-cosim_simulate_stage}"
NCU_LAUNCH_COUNT="${NCU_LAUNCH_COUNT:-20}"
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

    # The UM page-fault report NAMES differ between nsys versions (the hardcoded
    # cuda_um_{cpu,gpu}_page_faults_sum errored on nsys 2024.6.2 — "could not be
    # found"). So capture this nsys's report catalogue and DISCOVER the UM /
    # page-fault reports dynamically instead of guessing. Always-present summary
    # reports (kernel time, GPU mem-op time + size) are queried unconditionally.
    avail="$OUTDIR/nsys_reports_available.txt"
    nsys stats --help-reports 2>&1 | tee "$avail" >/dev/null || true
    reports=(cuda_gpu_kern_sum cuda_gpu_mem_time_sum cuda_gpu_mem_size_sum)
    # First token of each catalogue line is the report name. Match UM/page-fault
    # reports by name token, avoiding the "um" inside "...._sum" (preceded by 's',
    # not '_'/start, so (^|_)um($|_) won't match it). Strip the catalogue's
    # option-suffix annotations (e.g. `um_sum[:rows=<limit>]` -> `um_sum`) so the
    # bare report name reaches nsys.
    um_found=()
    while IFS= read -r r; do
        [ -n "$r" ] && { reports+=("$r"); um_found+=("$r"); }
    done < <(awk '{print $1}' "$avail" 2>/dev/null \
        | sed -e 's/\[.*//' -e 's/:.*//' \
        | grep -iE 'page_fault|unified|(^|_)um($|_)' | sort -u || true)
    if [ "${#um_found[@]}" -eq 0 ]; then
        echo "NOTE: no UM/page-fault nsys report found in this version's catalogue" \
             "($(basename "$avail")) — migration volume is inferred from the mem-op" \
             "summaries (UM migrations surface there as '[CUDA Unified Memory memcpy]')." \
             | tee -a "$OUTDIR/nsys_profile.log"
    fi

    stats_txt="$OUTDIR/nsys_stats.txt"
    report_args=()
    for r in "${reports[@]}"; do report_args+=(--report "$r"); done
    nsys stats "${report_args[@]}" "$rep.nsys-rep" 2>&1 | tee "$stats_txt" >/dev/null || true

    {
        echo "Report: \`$(basename "$rep").nsys-rep\` (full timeline, uploaded as artifact)."
        echo
        echo "UM/page-fault reports discovered in this nsys: ${um_found[*]:-none (see nsys_reports_available.txt)}"
        echo
        echo "Key \`nsys stats\` tables (kernel time, GPU mem-op time + size, any UM reports):"
        echo
        echo '```'
        # Trim to keep the step summary readable; full text in nsys_stats.txt.
        head -200 "$stats_txt" 2>/dev/null || echo "(nsys stats produced no output)"
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
    echo "=== Phase 3: ncu kernel profile ($NCU_EDGES edges, set=$NCU_SET, kernel~$NCU_KERNEL, launch-count=$NCU_LAUNCH_COUNT) ==="
    ncu_rep="$OUTDIR/flash_ncu"
    # Cloud GPUs restrict the HW counters to root (ERR_NVGPUCTRPERM), so run ncu
    # via passwordless sudo when available — root can read the counters. `-E`
    # preserves PATH/CUDA env so the profiled jacquard child still resolves the
    # CUDA runtime; CUDA libs resolve via the system ldconfig cache that root
    # shares. If sudo isn't passwordless (interactive use), fall back to a direct
    # run and degrade gracefully.
    ncu_bin="$(command -v ncu)"
    ncu_cmd=("$ncu_bin")
    if sudo -n true 2>/dev/null; then
        ncu_cmd=(sudo -E "$ncu_bin")
        echo "  (running ncu via sudo -E to access GPU performance counters)"
    fi
    # Bounded + targeted (see NCU_* rationale above): only the hotspot kernel,
    # only the first N launches, basic metric set. `--print-summary per-kernel`
    # emits a compact aggregate (Speed-of-Light, occupancy) to stdout.
    "${ncu_cmd[@]}" --set "$NCU_SET" --target-processes all \
        --kernel-name "regex:$NCU_KERNEL" --launch-count "$NCU_LAUNCH_COUNT" \
        --print-summary per-kernel \
        --export "$ncu_rep" -f \
        "$BIN" "${COSIM_ARGS[@]}" \
        --max-clock-edges "$NCU_EDGES" --output-vcd "$OUTDIR/mcu_flash_ncu.vcd" \
        2>&1 | tee "$OUTDIR/ncu_profile.log" >/dev/null || true

    {
        if grep -q 'ERR_NVGPUCTRPERM' "$OUTDIR/ncu_profile.log" 2>/dev/null; then
            echo "ncu could not read HW counters on this runner (ERR_NVGPUCTRPERM)"
            echo "even via sudo. The GPU needs profiling enabled — set the kernel-module"
            echo "param \`NVreg_RestrictProfilingToAdminUsers=0\` (or run the runner as root)."
        elif [ ! -f "$ncu_rep.ncu-rep" ]; then
            echo "ncu produced no report — check ncu_profile.log (no kernel matched"
            echo "\`$NCU_KERNEL\`, or the run errored)."
        else
            echo "Report: \`$(basename "$ncu_rep").ncu-rep\` (uploaded). Kernel: \`$NCU_KERNEL\`,"
            echo "$NCU_LAUNCH_COUNT launches, \`$NCU_SET\` metric set."
            echo
            echo "Per-kernel summary (Speed-of-Light / occupancy / duration):"
            echo
            echo '```'
            # The per-kernel aggregate ncu prints to stdout — the actionable bit.
            sed -n '/Section: GPU Speed Of Light/,/^$/p;/Duration/p;/Compute (SM)/p;/Memory Throughput/p;/Achieved Occupancy/p;/Achieved Active Warps/p' \
                "$OUTDIR/ncu_profile.log" 2>/dev/null | head -80 \
                || echo "(no summary parsed — see ncu_profile.log / the .ncu-rep)"
            echo '```'
        fi
    } >> "$SUMMARY"
fi

echo
echo "=== profile complete — artifacts in $OUTDIR/ ==="
echo "Summary written to $SUMMARY"

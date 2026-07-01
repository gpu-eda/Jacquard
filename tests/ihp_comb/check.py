#!/usr/bin/env python3
"""Independent truth-table check of the IHP SG13G2 descriptor logic.

Validates a `jacquard sim` output VCD for `ihp_comb_top.v` against hand-derived
boolean functions for each `sg13g2_*` combinational cell. This is stronger than
the built-in `--check-with-cpu` (which proves GPU == CPU on the *same* spliced
AIG): it proves the descriptor's combinational logic is actually CORRECT versus
the real IHP cell functions -- with zero IHP-specific Rust in Jacquard.

The design registers its primary inputs through AIGPDK DFFs (a clock harness),
so the combinational outputs `y_*` are functions of the registered inputs
`ra..rsel`. Run `jacquard sim` with

    --trace-signals <(printf 'ra\\nrb\\nrc\\nrd\\nre\\nrf\\nrsel\\n')

so those registered inputs appear in the output VCD; then `y_*` and `ra..rsel`
are sampled at the SAME timestamp (purely combinational -- no pipeline math).

Usage: check.py <output.vcd>
Exit 0 on PASS, non-zero on any mismatch.
"""
import re
import sys


def main(path: str) -> int:
    sig: dict[str, str] = {}
    val: dict[str, str] = {}
    checks = fails = sampled = 0

    def b(name):
        v = val.get(name, "x")
        return None if v in ("x", "z") else int(v)

    def expect():
        ra, rb, rc = b("ra"), b("rb"), b("rc")
        rd, re, rf, rs = b("rd"), b("re"), b("rf"), b("rsel")
        if (ra is None or rb is None or rc is None or rd is None
                or re is None or rf is None or rs is None):
            return None
        n_inv, n_buf = 1 - ra, rb
        w_nand, w_nor = 1 - (ra & rb), 1 - (rc | rd)
        t_xor = w_nand ^ w_nor
        t_mux = n_inv if rs else t_xor        # mux2: S ? A1 : A0
        return {
            "y_nand": 1 - (ra & rb),
            "y_nor":  1 - (rc | rd),
            "y_and":  ra & rc,
            "y_or":   rb | rd,
            "y_xor":  ra ^ re,
            "y_xnor": 1 - (rc ^ rf),
            "y_aoi":  1 - ((ra & rb) | rc),
            "y_a22":  1 - ((ra & rb) | (rc & rd)),
            "y_oai":  1 - ((n_inv | n_buf) & re),
            "y_mux":  rb if rs else ra,
            "y_deep": 1 - t_mux,
        }

    def check(ts):
        nonlocal checks, fails, sampled
        exp = expect()
        if exp is None:
            return
        sampled += 1
        for k, ev in exp.items():
            av = b(k)
            if av is None:
                continue
            checks += 1
            if av != ev:
                fails += 1
                if fails <= 10:
                    print(f"  MISMATCH @t={ts} {k}: got {av} exp {ev}")

    defn = True
    cur_ts = None
    with open(path) as fh:
        for ln in fh.read().splitlines():
            if defn:
                m = re.match(r"\$var wire 1 (\S+) (\S+) ", ln)
                if m:
                    sig[m.group(1)] = m.group(2)
                if ln.startswith("$enddefinitions"):
                    defn = False
                continue
            if ln.startswith("#"):
                if cur_ts is not None:
                    check(cur_ts)
                cur_ts = int(ln[1:])
                continue
            if ln in ("$dumpvars", "$end", "$dumpall"):
                continue
            m = re.match(r"([01xz])(\S+)$", ln)
            if m and m.group(2) in sig:
                val[sig[m.group(2)]] = m.group(1)
    if cur_ts is not None:
        check(cur_ts)

    print(f"sampled timestamps: {sampled}, signal checks: {checks}, "
          f"mismatches: {fails}")
    if fails == 0 and checks > 0:
        print("PASS -- IHP SG13G2 descriptor logic matches independent truth tables")
        return 0
    print("FAIL")
    return 1


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit("usage: check.py <output.vcd>")
    sys.exit(main(sys.argv[1]))

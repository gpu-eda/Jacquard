"""Tests for the xroots X-source frontier query (#98)."""

import json
import textwrap

import pytest
from click.testing import CliRunner

from netlist_graph.cli import main
from netlist_graph.graph import NetlistGraph


@pytest.fixture
def xsrc_netlist(tmp_path):
    """A design with a controlled X-source topology.

      out = and2(comb, undriven_w)
      comb = nand2(q_a, q_b)
      q_a  = unreset DFF (no reset pin)           <- persistent X-source
      q_b  = reset DFF (RESET_B), D = ext_in       <- traversed through
      undriven_w : no driver, not a port           <- persistent X-source
    """
    v = tmp_path / "design.v"
    v.write_text(textwrap.dedent("""\
        module top (clk, rst_n, ext_in, out);
         input clk;
         input rst_n;
         input ext_in;
         output out;
         wire q_a, q_b, comb, undriven_w;

         sky130_fd_sc_hd__dfxtp_1 ra (.CLK(clk), .D(comb), .Q(q_a));
         sky130_fd_sc_hd__dfrtp_1 rb (.CLK(clk), .RESET_B(rst_n), .D(ext_in), .Q(q_b));
         sky130_fd_sc_hd__nand2_1 g0 (.A(q_a), .B(q_b), .Y(comb));
         sky130_fd_sc_hd__and2_0  g1 (.A(comb), .B(undriven_w), .X(out));
        endmodule
    """))
    return v


class TestNativeXSources:
    def test_classification(self, xsrc_netlist):
        g = NetlistGraph.from_file(xsrc_netlist)
        src = g.native_x_sources()
        assert src.get("q_a") == "unreset-dff"
        assert src.get("q_b") == "reset-dff"
        assert src.get("undriven_w") == "undriven-input"
        # ext_in is a primary input (driven externally) — not an X-source here.
        assert "ext_in" not in src


class TestFrontier:
    def test_stops_at_persistent_sources(self, xsrc_netlist):
        g = NetlistGraph.from_file(xsrc_netlist)
        stop = {"q_a", "undriven_w"}  # persistent roots; q_b (reset) traversed
        frontier = g.x_source_frontier("out", stop)
        nets = {n for _, n in frontier}
        assert nets == {"q_a", "undriven_w"}

    def test_traverses_through_reset_dff(self, xsrc_netlist):
        g = NetlistGraph.from_file(xsrc_netlist)
        # q_b is a reset DFF: not a stop net, so the walk continues through its
        # D pin to ext_in. With ext_in marked persistent, it becomes a root.
        stop = {"q_a", "undriven_w", "ext_in"}
        frontier = g.x_source_frontier("out", stop)
        nets = {n for _, n in frontier}
        assert "ext_in" in nets  # reached by traversing through the reset DFF

    def test_nearest_first(self, xsrc_netlist):
        g = NetlistGraph.from_file(xsrc_netlist)
        frontier = g.x_source_frontier("out", {"q_a", "undriven_w"})
        depths = [d for d, _ in frontier]
        assert depths == sorted(depths)

    def test_does_not_walk_clock_pin(self, xsrc_netlist):
        g = NetlistGraph.from_file(xsrc_netlist)
        # clk drives the DFF CLK pins; the frontier walk must not descend into
        # the clock, so clk never appears even if marked an X-source.
        frontier = g.x_source_frontier("out", {"clk", "q_a"})
        nets = {n for _, n in frontier}
        assert "clk" not in nets


class TestXrootsCli:
    def test_native_frontier(self, xsrc_netlist):
        runner = CliRunner()
        result = runner.invoke(main, ["xroots", str(xsrc_netlist), "out"])
        assert result.exit_code == 0, result.output
        assert "[unreset-dff] q_a" in result.output
        assert "[undriven-input] undriven_w" in result.output
        # reset DFF is traversed through, not reported as a root.
        assert "q_b" not in result.output

    def test_manifest_resolution_by_cell(self, xsrc_netlist, tmp_path):
        # A manifest naming the source net `CANON_qa` (a name absent from the
        # netlist) but with the correct driving cell `ra` must still resolve,
        # via cell→net, to the graph's `q_a`.
        manifest = tmp_path / "xsrc.json"
        manifest.write_text(json.dumps({
            "schema_version": "1.0",
            "netlist": str(xsrc_netlist),
            "undriven_inputs_classified": True,
            "x_sources": [
                {"net": "CANON_qa", "kind": "unreset-dff", "cell": "ra"},
                {"net": "undriven_w", "kind": "undriven-input"},
            ],
        }))
        runner = CliRunner()
        result = runner.invoke(
            main, ["xroots", str(xsrc_netlist), "out", "--xsources", str(manifest)]
        )
        assert result.exit_code == 0, result.output
        # Resolved by cell despite the mismatched net name.
        assert "[unreset-dff] q_a" in result.output
        assert "[undriven-input] undriven_w" in result.output

    def test_emit_trace_roundtrips(self, xsrc_netlist, tmp_path):
        out = tmp_path / "trace.txt"
        runner = CliRunner()
        result = runner.invoke(
            main, ["xroots", str(xsrc_netlist), "out", "--emit-trace", str(out)]
        )
        assert result.exit_code == 0, result.output
        content = out.read_text()
        # One bare net name per non-comment line, resolvable in the netlist.
        nets = [ln for ln in content.splitlines() if ln and not ln.startswith("#")]
        assert "q_a" in nets
        assert "undriven_w" in nets

    def test_signal_is_itself_a_source(self, xsrc_netlist):
        runner = CliRunner()
        result = runner.invoke(main, ["xroots", str(xsrc_netlist), "q_a"])
        assert result.exit_code == 0, result.output
        assert "is itself an X-source" in result.output

    def test_unknown_signal_errors(self, xsrc_netlist):
        runner = CliRunner()
        result = runner.invoke(main, ["xroots", str(xsrc_netlist), "nonexistent_xyz"])
        assert result.exit_code == 1

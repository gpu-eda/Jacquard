"""Tests for cone (#99) and drivers --data-only (#100) fixes."""

import textwrap

import pytest
from click.testing import CliRunner

from netlist_graph.cli import main
from netlist_graph.graph import NetlistGraph
from netlist_graph.parser import classify_pin, parse_netlist, parse_top_ports, pin_is_recognized


class TestPinClassification:
    """#99: output pins must not be misclassified as inputs."""

    @pytest.mark.parametrize(
        "pin",
        ["X", "Y", "Z", "ZN", "Q", "QN", "HI", "LO", "CO", "COUT", "SUM"],
    )
    def test_output_pins(self, pin):
        assert classify_pin(pin) == "output", f"{pin} should be an output"

    @pytest.mark.parametrize("pin", ["A", "B", "A1", "B2", "CLK", "RESET_B", "S", "S0", "D"])
    def test_input_pins(self, pin):
        assert classify_pin(pin) == "input", f"{pin} should be an input"

    def test_unknown_pin_warns(self, tmp_path, caplog):
        """An unrecognized pin name surfaces as a warning, not a silent default."""
        assert not pin_is_recognized("WEIRDOUT")
        v = tmp_path / "weird.v"
        v.write_text(
            "module top (a, o);\n input a; output o;\n"
            " somecell c0 (.A(a), .WEIRDOUT(o));\nendmodule\n"
        )
        import logging

        with caplog.at_level(logging.WARNING):
            parse_netlist(v)
        assert any("WEIRDOUT" in r.message for r in caplog.records)


class TestTopPortParsing:
    def test_scalar_and_bus(self):
        content = textwrap.dedent("""\
            module top (por_l, gpio_in, gpio_out, io);
             input por_l;
             input [3:0] gpio_in;
             output [1:0] gpio_out;
             inout [43:0] io;
            endmodule
        """)
        ports = parse_top_ports(content)
        assert ports["por_l"] == "input"
        assert ports["gpio_in[0]"] == "input"
        assert ports["gpio_in[3]"] == "input"
        assert ports["gpio_out[1]"] == "output"
        assert ports["io[43]"] == "inout"


@pytest.fixture
def driven_net_netlist(tmp_path):
    """A register whose D is a hold mux fed by combinational cells.

    Reproduces #99: with a conb tie cell and an inverting (.ZN) cell whose
    outputs were previously misclassified as inputs, leaving the nets looking
    undriven and mislabeled [primary input].
    """
    v = tmp_path / "design.v"
    v.write_text(textwrap.dedent("""\
        module top (clk, sel_in, dout);
         input clk;
         input sel_in;
         output dout;
         wire d_net, mux_o, hi_net, zn_net, undriven_net;

         sky130_fd_sc_hd__conb_1 tie (.HI(hi_net));
         sky130_fd_sc_hd__aoi21_1 g0 (.A1(hi_net), .A2(sel_in), .B1(undriven_net), .ZN(zn_net));
         sky130_fd_sc_hd__mux2_1  mx (.A0(zn_net), .A1(dout), .S(hi_net), .X(mux_o));
         sky130_fd_sc_hd__dfxtp_1 reg0 (.CLK(clk), .D(mux_o), .Q(dout));
        endmodule
    """))
    return v


class TestConeDrivenNets:
    """#99: cone should expand driven internal nets, not stop at them."""

    def test_zn_output_is_driven(self, driven_net_netlist):
        g = NetlistGraph.from_file(driven_net_netlist)
        # zn_net is driven by the aoi21 .ZN output → must have a driver edge.
        assert g.find_drivers("zn_net"), "ZN output net should be driven"
        assert g.is_driven("zn_net")
        # hi_net is driven by the conb tie cell .HI output. A tie cell has no
        # inputs so there is no driver *edge*, but the net is still driven.
        assert not g.find_drivers("hi_net")
        assert g.is_driven("hi_net"), "tie-cell output should count as driven"
        assert g.driver_cell_type("hi_net") == "sky130_fd_sc_hd__conb_1"

    def test_undriven_internal_net_flagged_distinctly(self, driven_net_netlist):
        g = NetlistGraph.from_file(driven_net_netlist)
        # undriven_net has no driver and is not a top-level port → X-source.
        assert not g.find_drivers("undriven_net")
        assert not g.is_primary_input("undriven_net")
        # sel_in is a real primary input.
        assert g.is_primary_input("sel_in")

    def test_cone_cli_labels(self, driven_net_netlist):
        runner = CliRunner()
        result = runner.invoke(main, ["cone", str(driven_net_netlist), "dout"])
        assert result.exit_code == 0, result.output
        # zn_net must be expanded, not labeled [primary input].
        assert "zn_net = aoi21_1" in result.output or "zn_net" in result.output
        # the genuinely undriven net is flagged as an X-source, not primary.
        assert "undriven_net  [undriven — X-source]" in result.output
        # the real primary input keeps its label.
        assert "sel_in  [primary input]" in result.output
        # the driven internal net must NOT be mislabeled as primary.
        assert "zn_net  [primary input]" not in result.output


@pytest.fixture
def reg_with_reset_netlist(tmp_path):
    """A register with a clock and reset tree feeding its control pins."""
    v = tmp_path / "reg.v"
    v.write_text(textwrap.dedent("""\
        module top (clk_pad, rst_pad, d_in, q_out);
         input clk_pad;
         input rst_pad;
         input d_in;
         output q_out;
         wire clk_buf, rst_sync, data_net;

         sky130_fd_sc_hd__clkbuf_16 cb (.A(clk_pad), .X(clk_buf));
         sky130_fd_sc_hd__dfxtp_1   rs (.CLK(clk_buf), .D(rst_pad), .Q(rst_sync));
         sky130_fd_sc_hd__buf_1     db (.A(d_in), .X(data_net));
         sky130_fd_sc_hd__dfrtp_1   r0 (.CLK(clk_buf), .RESET_B(rst_sync), .D(data_net), .Q(q_out));
        endmodule
    """))
    return v


class TestDriversDataOnly:
    """#100: --data-only skips clk/reset pins on sequential cells."""

    def test_default_walks_clock_and_reset(self, reg_with_reset_netlist):
        g = NetlistGraph.from_file(reg_with_reset_netlist)
        trace = g.trace_back("q_out", max_depth=10)
        nets = {n for _, n, _ in trace}
        # Default walk reaches the clock and reset trees.
        assert "clk_buf" in nets
        assert "rst_sync" in nets
        assert "data_net" in nets

    def test_data_only_skips_control(self, reg_with_reset_netlist):
        g = NetlistGraph.from_file(reg_with_reset_netlist)
        trace = g.trace_back("q_out", max_depth=10, data_only=True)
        nets = {n for _, n, _ in trace}
        # Data path is kept ...
        assert "data_net" in nets
        assert "d_in" in nets
        # ... but the clock and reset trees of r0 are not descended into.
        assert "clk_buf" not in nets
        assert "rst_sync" not in nets

    def test_cli_data_only_flag(self, reg_with_reset_netlist):
        runner = CliRunner()
        result = runner.invoke(
            main, ["drivers", str(reg_with_reset_netlist), "q_out", "--data-only"]
        )
        assert result.exit_code == 0, result.output
        assert "data path only" in result.output
        assert "clk_buf" not in result.output
        assert "rst_sync" not in result.output

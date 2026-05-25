"""Tests for sram-ports command and parse_cell_instances."""

import textwrap

import pytest
from click.testing import CliRunner

from netlist_graph.cli import main
from netlist_graph.parser import parse_cell_instances


@pytest.fixture
def tmp_netlist(tmp_path):
    """Create a minimal netlist with one SRAM and one logic cell."""
    v = tmp_path / "design.v"
    v.write_text(textwrap.dedent("""\
        module top (input clk, input [9:0] addr, input [31:0] din,
                    output [31:0] dout, input wen, input en);
         wire net_clk, net_en, net_wen;
         wire [9:0] net_addr;
         wire [31:0] net_din, net_dout;

         sky130_fd_sc_hd__buf_2 buf0 (.A(clk), .X(net_clk));

         CF_SRAM_1024x32 sram0 (
             .CLKin(net_clk),
             .EN(net_en),
             .R_WB(net_wen),
             .AD({addr[9], addr[8], addr[7], addr[6], addr[5],
                  addr[4], addr[3], addr[2], addr[1], addr[0]}),
             .DI({din[31], din[30], din[29], din[28], din[27],
                  din[26], din[25], din[24], din[23], din[22],
                  din[21], din[20], din[19], din[18], din[17],
                  din[16], din[15], din[14], din[13], din[12],
                  din[11], din[10], din[9], din[8], din[7],
                  din[6], din[5], din[4], din[3], din[2],
                  din[1], din[0]}),
             .DO({dout[31], dout[30], dout[29], dout[28], dout[27],
                  dout[26], dout[25], dout[24], dout[23], dout[22],
                  dout[21], dout[20], dout[19], dout[18], dout[17],
                  dout[16], dout[15], dout[14], dout[13], dout[12],
                  dout[11], dout[10], dout[9], dout[8], dout[7],
                  dout[6], dout[5], dout[4], dout[3], dout[2],
                  dout[1], dout[0]}));
        endmodule
    """))
    return v


@pytest.fixture
def tmp_manifest(tmp_path):
    """Create a minimal .cells.toml with RAM port mapping."""
    toml = tmp_path / "lib.cells.toml"
    toml.write_text(textwrap.dedent("""\
        schema_version = "1.1"

        [cells.CF_SRAM_1024x32]
        kind = "ram"

        [cells.CF_SRAM_1024x32.ram]
        depth = 1024
        width = 32
        address = "AD"
        data_in = "DI"
        data_out = "DO"
        [cells.CF_SRAM_1024x32.ram.clock]
        pin = "CLKin"
        [cells.CF_SRAM_1024x32.ram.chip_enable]
        pin = "EN"
        polarity = "low"
        [cells.CF_SRAM_1024x32.ram.write_enable]
        pin = "R_WB"
        polarity = "low"
    """))
    return toml


class TestParseCellInstances:
    def test_finds_sram_and_logic(self, tmp_netlist):
        cells = parse_cell_instances(tmp_netlist)
        types = {c.cell_type for c in cells}
        assert "CF_SRAM_1024x32" in types
        assert "sky130_fd_sc_hd__buf_2" in types

    def test_sram_ports_present(self, tmp_netlist):
        cells = parse_cell_instances(tmp_netlist)
        sram = [c for c in cells if c.cell_type == "CF_SRAM_1024x32"][0]
        assert "CLKin" in sram.ports
        assert "AD" in sram.ports
        assert "DI" in sram.ports
        assert "DO" in sram.ports
        assert "EN" in sram.ports
        assert "R_WB" in sram.ports
        assert len(sram.ports["AD"]) == 10
        assert len(sram.ports["DI"]) == 32
        assert len(sram.ports["DO"]) == 32

    def test_instance_name(self, tmp_netlist):
        cells = parse_cell_instances(tmp_netlist)
        sram = [c for c in cells if c.cell_type == "CF_SRAM_1024x32"][0]
        assert sram.inst_name == "sram0"


class TestSramPortsCli:
    def test_cell_type_pattern(self, tmp_netlist):
        runner = CliRunner()
        result = runner.invoke(main, ["sram-ports", str(tmp_netlist), "-c", "SRAM"])
        assert result.exit_code == 0
        assert "addr[0]" in result.output
        assert "din[0]" in result.output
        assert "dout[0]" in result.output

    def test_manifest_mode(self, tmp_netlist, tmp_manifest):
        runner = CliRunner()
        result = runner.invoke(
            main,
            ["sram-ports", str(tmp_netlist), "--manifest", str(tmp_manifest)],
        )
        assert result.exit_code == 0
        lines = [ln for ln in result.output.strip().split("\n") if not ln.startswith("#")]
        pin_names_in_comments = [
            ln for ln in result.output.strip().split("\n") if ln.startswith("#")
        ]
        assert len(lines) > 0
        assert any("AD" in c for c in pin_names_in_comments)
        assert any("DI" in c for c in pin_names_in_comments)
        assert any("DO" in c for c in pin_names_in_comments)
        assert any("EN" in c for c in pin_names_in_comments)
        assert any("R_WB" in c for c in pin_names_in_comments)

    def test_no_match_exits_with_error(self, tmp_netlist):
        runner = CliRunner()
        result = runner.invoke(main, ["sram-ports", str(tmp_netlist)])
        assert result.exit_code == 1

    def test_output_file(self, tmp_netlist, tmp_path):
        runner = CliRunner()
        out = tmp_path / "trace.txt"
        result = runner.invoke(
            main,
            ["sram-ports", str(tmp_netlist), "-c", "SRAM", "-o", str(out)],
        )
        assert result.exit_code == 0
        content = out.read_text()
        assert "addr[0]" in content

    def test_builtin_ramgem(self, tmp_path):
        """$__RAMGEM_SYNC_ is detected without --manifest."""
        v = tmp_path / "ramgem.v"
        v.write_text(textwrap.dedent("""\
            module top;
             $__RAMGEM_SYNC_ ram0 (
                 .PORT_W_CLK(clk),
                 .PORT_R_CLK(clk),
                 .PORT_W_ADDR(waddr),
                 .PORT_R_ADDR(raddr),
                 .PORT_W_WR_EN(wen),
                 .PORT_W_WR_DATA(wdata),
                 .PORT_R_RD_DATA(rdata));
            endmodule
        """))
        runner = CliRunner()
        result = runner.invoke(main, ["sram-ports", str(v)])
        assert result.exit_code == 0
        assert "waddr" in result.output
        assert "wdata" in result.output
        assert "rdata" in result.output

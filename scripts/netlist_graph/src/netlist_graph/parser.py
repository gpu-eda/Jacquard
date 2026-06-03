"""Parse Verilog netlists into graph structures."""

import logging
import re
from dataclasses import dataclass
from pathlib import Path

import networkx as nx

logger = logging.getLogger(__name__)

# Standard-cell output pin names, unambiguous across the libraries we parse
# (sky130, gf180mcu, Nangate/generic). These names are NEVER inputs on any
# cell, so a name-based allowlist is safe. Adding an ambiguous name here
# (e.g. ``S``, which is an adder *sum* output but also a mux *select* input)
# would misclassify edges, so keep this list to output-only names.
#   X, Y, Z, ZN      — combinational outputs (buf/inv/aoi/oai/nand/nor)
#   Q, QN, Q_N       — flop outputs
#   SUM, CO, COUT    — adder outputs (sky130 uses SUM/COUT, gf180 uses S/CO —
#                      ``S`` is intentionally omitted, see above)
#   HI, LO           — sky130 conb_1 tie cell outputs
#   DO               — SRAM data-out
#   GCLK, ECK, CLKOUT — clock-gate / clock-buffer outputs
OUTPUT_PINS = {
    "X", "Y", "Z", "ZN",
    "Q", "QN", "Q_N",
    "SUM", "CO", "COUT",
    "HI", "LO",
    "DO",
    "GCLK", "ECK", "CLKOUT",
}
INPUT_PINS = {
    "A", "B", "C", "D", "A1", "A2", "A3", "B1", "B2", "C1", "D1",
    "S", "S0", "CLK", "RESET_B", "SET_B", "GATE", "EN",
    "A0", "DI", "AD", "BEN", "CLKin", "R_WB", "WLBI", "SM", "TM",
    "ScanInDR", "ScanInDL", "ScanInCC", "vpwrpc", "vpwrac",
}

# Top-level port declaration:  input|output|inout [msb:lsb] name1, name2, ...;
PORT_DECL_PATTERN = re.compile(
    r"^\s*(input|output|inout)\s+"
    r"(?:\[(\d+)\s*:\s*(\d+)\]\s*)?"  # optional bus range
    r"([^;]+);",
    re.MULTILINE,
)

# Cell pattern: cell_type instance_name (.port(net), ...);
CELL_PATTERN = re.compile(
    r"(\$?[\w]+)\s+"  # cell type (may start with $ for Yosys internal cells)
    r"(\\[^\s]+|\w+)\s*"  # instance name (escaped or simple)
    r"\(([^;]+)\);",  # port connections
    re.MULTILINE | re.DOTALL,
)

# Port connection pattern
PORT_PATTERN = re.compile(r"\.(\w+)\s*\(([^)]*)\)")


def classify_pin(pin_name: str) -> str:
    """Classify a pin as 'input' or 'output'."""
    pin_base = pin_name.rstrip("0123456789")
    if pin_base in OUTPUT_PINS or pin_name in OUTPUT_PINS:
        return "output"
    if pin_base in INPUT_PINS or pin_name in INPUT_PINS:
        return "input"
    # Guess based on common patterns
    if pin_name.startswith("Q") or pin_name in ("X", "Y", "Z"):
        return "output"
    return "input"  # Default to input


def pin_is_recognized(pin_name: str) -> bool:
    """Whether ``classify_pin`` resolves this pin name confidently.

    Pin direction is inferred from a name-based allowlist (no Liberty file is
    available). An unrecognized pin silently defaults to ``input`` — which, if
    the pin is really an output, makes its net look undriven (the #99 failure
    mode, relocated to the next cell library). ``parse_netlist`` uses this to
    warn about unknown pin names so a new PDK surfaces as a diagnostic rather
    than a silent misclassification. Mirrors the branches of ``classify_pin``.
    """
    pin_base = pin_name.rstrip("0123456789")
    if pin_base in OUTPUT_PINS or pin_name in OUTPUT_PINS:
        return True
    if pin_base in INPUT_PINS or pin_name in INPUT_PINS:
        return True
    return pin_name.startswith("Q") or pin_name in ("X", "Y", "Z")


def parse_top_ports(content: str) -> dict[str, str]:
    """Parse top-level module port declarations.

    Returns a mapping of (bit-expanded) net name -> direction, where
    direction is one of ``"input"``, ``"output"``, ``"inout"``. Bus ports
    are expanded to match how nets appear in cell connections, e.g.
    ``input [43:0] gpio_in`` yields ``gpio_in[0] .. gpio_in[43]``.

    Used to distinguish a genuine top-level primary input (driven from
    outside the module) from an *undriven internal net* (an X-source). Both
    have no driver inside the netlist, but only the latter is a defect/X-root.
    """
    ports: dict[str, str] = {}
    for m in PORT_DECL_PATTERN.finditer(content):
        direction = m.group(1)
        msb, lsb = m.group(2), m.group(3)
        names_blob = m.group(4)
        # Strip any leading data-type keyword (wire/reg) and whitespace, then
        # split the comma-separated name list.
        names_blob = re.sub(r"\b(wire|reg|logic|signed)\b", " ", names_blob)
        for raw in names_blob.split(","):
            name = raw.strip().lstrip("\\").rstrip(" ")
            if not name:
                continue
            if msb is not None and lsb is not None:
                hi, lo = int(msb), int(lsb)
                step = 1 if hi >= lo else -1
                for bit in range(lo, hi + step, step):
                    ports[f"{name}[{bit}]"] = direction
                # also record the bare name for scalar-style references
                ports.setdefault(name, direction)
            else:
                ports[name] = direction
    return ports


def extract_nets(net_expr: str) -> list[str]:
    """Extract individual net names from a port expression."""
    net_expr = net_expr.strip()
    if not net_expr:
        return []

    if net_expr.startswith("{"):
        # Concatenation: {a, b, c}
        inner = net_expr[1:-1]
        nets = re.findall(r"\\[^\s,}]+|\w+\[[^\]]+\]|\w+", inner)
        return [n.strip() for n in nets if n.strip()]
    else:
        return [net_expr]


def parse_netlist(netlist_path: Path) -> nx.DiGraph:
    """
    Parse a Verilog netlist and build a directed graph.

    Nodes represent nets/wires.
    Edges represent signal flow through cells (input -> output).
    Edge attributes include cell instance name and type.
    """
    G = nx.DiGraph()
    content = netlist_path.read_text()

    # Top-level port directions, used to tell a real primary input from an
    # undriven internal net (an X-source). Stored on the graph so query
    # methods can classify leaf nets without re-reading the file.
    G.graph["ports"] = parse_top_ports(content)

    # net -> (instance, cell_type, out_pin) for every net that is some cell's
    # output. Distinguishes a driven net (even via a zero-input tie cell) from
    # a genuinely undriven net. Stored on the graph for query methods.
    out_driver: dict[str, tuple[str, str, str]] = {}
    G.graph["out_driver"] = out_driver

    cells_parsed = 0
    skip_types = {"wire", "input", "output", "inout", "module", "assign", "endmodule"}
    unknown_pins: set[str] = set()

    for match in CELL_PATTERN.finditer(content):
        cell_type = match.group(1)
        inst_name = match.group(2).strip()
        ports_str = match.group(3)

        if cell_type in skip_types:
            continue

        cells_parsed += 1

        # Parse port connections
        inputs: list[tuple[str, str]] = []   # (net, pin_name)
        outputs: list[tuple[str, str]] = []  # (net, pin_name)

        for port_match in PORT_PATTERN.finditer(ports_str):
            pin_name = port_match.group(1)
            net_expr = port_match.group(2)
            nets = extract_nets(net_expr)

            if not pin_is_recognized(pin_name):
                unknown_pins.add(pin_name)
            pin_type = classify_pin(pin_name)
            if pin_type == "output":
                for net in nets:
                    outputs.append((net, pin_name))
            else:
                for net in nets:
                    inputs.append((net, pin_name))

        # Add nodes
        for net, _pin in inputs + outputs:
            if net and not G.has_node(net):
                G.add_node(net, type="net")

        # Record every output net's driving cell. This captures nets driven
        # by zero-input cells (constant/tie cells like sky130 conb_1, whose
        # HI/LO outputs produce no input->output edge), so they are not
        # mistaken for undriven X-sources downstream.
        for out, out_pin in outputs:
            if out:
                out_driver[out] = (inst_name, cell_type, out_pin)

        # Create edges from inputs to outputs through this cell
        for inp, inp_pin in inputs:
            for out, out_pin in outputs:
                if inp and out:
                    G.add_edge(
                        inp, out,
                        cell=inst_name, cell_type=cell_type,
                        in_pin=inp_pin, out_pin=out_pin,
                    )

    logger.info(f"Parsed {cells_parsed} cells")
    logger.info(f"Graph has {G.number_of_nodes()} nodes, {G.number_of_edges()} edges")

    if unknown_pins:
        logger.warning(
            "%d unrecognized pin name(s) defaulted to 'input': %s. If any is "
            "actually an output, its net will look undriven — add it to "
            "OUTPUT_PINS in parser.py.",
            len(unknown_pins),
            ", ".join(sorted(unknown_pins)),
        )

    return G


@dataclass
class CellInstance:
    cell_type: str
    inst_name: str
    ports: dict[str, list[str]]


def parse_cell_instances(netlist_path: Path) -> list[CellInstance]:
    """Parse all cell instances from a netlist, preserving port→net mappings."""
    content = netlist_path.read_text()
    skip_types = {"wire", "input", "output", "inout", "module", "assign", "endmodule"}
    cells = []
    for match in CELL_PATTERN.finditer(content):
        cell_type = match.group(1)
        if cell_type in skip_types:
            continue
        inst_name = match.group(2).strip()
        ports_str = match.group(3)
        ports: dict[str, list[str]] = {}
        for port_match in PORT_PATTERN.finditer(ports_str):
            pin_name = port_match.group(1)
            nets = extract_nets(port_match.group(2))
            ports[pin_name] = nets
        cells.append(CellInstance(cell_type, inst_name, ports))
    return cells

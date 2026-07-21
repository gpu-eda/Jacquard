// Populate the sidebar
//
// This is a script, and not included directly in the page, to control the total size of the book.
// The TOC contains an entry for each page, so if each page includes a copy of the TOC,
// the total size of the page becomes O(n**2).
class MDBookSidebarScrollbox extends HTMLElement {
    constructor() {
        super();
    }
    connectedCallback() {
        this.innerHTML = '<ol class="chapter"><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="index.html"><strong aria-hidden="true">1.</strong> Introduction</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="project-scope.html"><strong aria-hidden="true">2.</strong> Project Scope &amp; Guarantees</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="installation.html"><strong aria-hidden="true">3.</strong> Installation</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="getting-started.html"><strong aria-hidden="true">4.</strong> Getting Started</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="input-netlist.html"><strong aria-hidden="true">5.</strong> Input: Netlist Language &amp; RTL</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="accepted-rtl.html"><strong aria-hidden="true">6.</strong> Accepted RTL Surface</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="synthesis-flow.html"><strong aria-hidden="true">7.</strong> Synthesis Flow</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="interop.html"><strong aria-hidden="true">8.</strong> Testbench Interop</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="why-jacquard.html"><strong aria-hidden="true">9.</strong> Why Jacquard</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="simulation-architecture.html"><strong aria-hidden="true">10.</strong> Simulation Architecture</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="timing-correctness.html"><strong aria-hidden="true">11.</strong> Timing Correctness</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="timing-simulation.html"><strong aria-hidden="true">12.</strong> Timing Simulation</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="timing-violations.html"><strong aria-hidden="true">13.</strong> Timing Violations</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="timing-validation.html"><strong aria-hidden="true">14.</strong> Timing Validation</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="timing-model-extensions.html"><strong aria-hidden="true">15.</strong> Timing Model Extensions</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="signal-tracing.html"><strong aria-hidden="true">16.</strong> Signal Tracing</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="bus-tracing.html"><strong aria-hidden="true">17.</strong> Bus Transaction Tracing</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="cosim-perf-report.html"><strong aria-hidden="true">18.</strong> Cosim Perf Report</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="gpu-capture.html"><strong aria-hidden="true">19.</strong> GPU Frame Capture</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="selective-x-propagation.html"><strong aria-hidden="true">20.</strong> Selective X-Propagation</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="x-debugging.html"><strong aria-hidden="true">21.</strong> Debugging X Values</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="jtag-debug.html"><strong aria-hidden="true">22.</strong> Interactive JTAG Debug</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="adding-a-pdk.html"><strong aria-hidden="true">23.</strong> Adding a New PDK</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="troubleshooting-vcd.html"><strong aria-hidden="true">24.</strong> Troubleshooting VCD</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="architecture-diagrams.html"><strong aria-hidden="true">25.</strong> Authoring Architecture Diagrams</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="development.html"><strong aria-hidden="true">26.</strong> Development</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="release-process.html"><strong aria-hidden="true">27.</strong> Release Process</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="handoff-discipline.html"><strong aria-hidden="true">28.</strong> Handoff Discipline</a></span></li><li class="chapter-item expanded "><li class="part-title">Decisions &amp; plans</li></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="adr/index.html"><strong aria-hidden="true">29.</strong> Architecture Decision Records</a></span><ol class="section"><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="adr/0001-opensta-as-oracle.html"><strong aria-hidden="true">29.1.</strong> 0001 — OpenSTA as the timing correctness oracle</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="adr/0002-timing-ir.html"><strong aria-hidden="true">29.2.</strong> 0002 — Timing intermediate representation</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="adr/0003-opentimer-primary-sta.html"><strong aria-hidden="true">29.3.</strong> 0003 — OpenTimer as in-process reference STA (Superseded)</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="adr/0004-private-pdk-testing.html"><strong aria-hidden="true">29.4.</strong> 0004 — Private PDK testing track</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="adr/0005-opensta-vendoring-and-corpus.html"><strong aria-hidden="true">29.5.</strong> 0005 — OpenSTA vendoring and test-corpus strategy</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="adr/0006-sdf-preprocessing-model.html"><strong aria-hidden="true">29.6.</strong> 0006 — SDF preprocessing model</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="adr/0007-timing-model-fidelity-roadmap.html"><strong aria-hidden="true">29.7.</strong> 0007 — Timing model fidelity roadmap (Proposed)</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="adr/0008-structured-timing-output.html"><strong aria-hidden="true">29.8.</strong> 0008 — Structured timing output as first-class deliverable</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="adr/0009-opensta-verilog-reader-inputs.html"><strong aria-hidden="true">29.9.</strong> 0009 — OpenSTA Verilog reader input constraints</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="adr/0010-declarative-cell-metadata.html"><strong aria-hidden="true">29.10.</strong> 0010 — Declarative cell metadata for PDK enablement</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="adr/0011-ram-port-mapping-schema.html"><strong aria-hidden="true">29.11.</strong> 0011 — RAM port-mapping schema for declarative cell metadata</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="adr/0012-cdc-jitter-injection.html"><strong aria-hidden="true">29.12.</strong> 0012 — Reproducible CDC jitter injection for multi-clock cosim</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="adr/0013-plural-peripheral-configs.html"><strong aria-hidden="true">29.13.</strong> 0013 — Cosim peripheral model architecture</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="adr/0014-aig-as-simulation-ir.html"><strong aria-hidden="true">29.14.</strong> 0014 — AIG as simulation intermediate representation</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="adr/0015-boomerang-execution-model.html"><strong aria-hidden="true">29.15.</strong> 0015 — Boomerang execution model and GPU resource mapping</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="adr/0016-selective-x-propagation.html"><strong aria-hidden="true">29.16.</strong> 0016 — Selective X-propagation</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="adr/0017-cosim-execution-model.html"><strong aria-hidden="true">29.17.</strong> 0017 — Cosim execution model</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="adr/0018-distribution-and-installation.html"><strong aria-hidden="true">29.18.</strong> 0018 — Distribution and installation model (Proposed)</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="adr/0019-cell-model-ir.html"><strong aria-hidden="true">29.19.</strong> 0019 — Cell-model IR: a complete per-cell-type library descriptor (Proposed)</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="adr/0020-python-engine-binary-wheel.html"><strong aria-hidden="true">29.20.</strong> 0020 — Python engine as a bundled binary wheel (Draft — deferred)</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="adr/0021-behavioral-rtl-support.html"><strong aria-hidden="true">29.21.</strong> 0021 — Behavioral RTL support via an embedded synthesis front-end (Proposed)</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="adr/0022-flow-controlled-io.html"><strong aria-hidden="true">29.22.</strong> 0022 — Flow-controlled external I/O across the batch boundary (Proposed)</a></span></li></ol><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="plans/index.html"><strong aria-hidden="true">30.</strong> Implementation Plans</a></span><ol class="section"><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="plans/post-phase-0-roadmap.html"><strong aria-hidden="true">30.1.</strong> Post-Phase-0 Roadmap</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="plans/gf180mcu-enablement.html"><strong aria-hidden="true">30.2.</strong> GF180MCU PDK enablement</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="plans/phase-0-ir-and-oracle.html"><strong aria-hidden="true">30.3.</strong> Phase 0: Timing IR and OpenSTA oracle</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="plans/ws2-opensta-to-ir.html"><strong aria-hidden="true">30.4.</strong> WS2: opensta-to-ir</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="plans/ws3-delete-sdf-parser.html"><strong aria-hidden="true">30.5.</strong> WS3: delete SDF parser</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="plans/ws3-cosim-sdf-followup.html"><strong aria-hidden="true">30.6.</strong> WS3 follow-up: cosim --sdf</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="plans/multi-clock-and-stimulus-architecture.html"><strong aria-hidden="true">30.7.</strong> Multi-clock and stimulus architecture</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="plans/declarative-cell-metadata.html"><strong aria-hidden="true">30.8.</strong> Declarative cell metadata</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="plans/multi-peripheral-cosim.html"><strong aria-hidden="true">30.9.</strong> Multi-peripheral cosim</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="plans/bus-transaction-tracing.html"><strong aria-hidden="true">30.10.</strong> Bus transaction tracing</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="plans/cdc-jitter-completion.html"><strong aria-hidden="true">30.11.</strong> CDC jitter completion</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="plans/distribution.html"><strong aria-hidden="true">30.12.</strong> Distribution &amp; easy install</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="plans/cosim-xprop.html"><strong aria-hidden="true">30.13.</strong> Selective X-propagation in cosim</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="plans/cosim-backend-portability.html"><strong aria-hidden="true">30.14.</strong> Cosim backend portability</a></span><ol class="section"><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="plans/cosim-phase1-cpu-backend.html"><strong aria-hidden="true">30.14.1.</strong> Phase 1 — CpuBackend + Linux cosim CI</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="plans/cosim-phase2-cuda-hip.html"><strong aria-hidden="true">30.14.2.</strong> Phase 2 — CUDA/HIP parity</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="plans/cosim-phase2-stageB-io-port.html"><strong aria-hidden="true">30.14.3.</strong> Phase 2 Stage B — gpu_io_step port</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="plans/cosim-phase2-stageC-flash.html"><strong aria-hidden="true">30.14.4.</strong> Phase 2 Stage C — flash kernels</a></span></li></ol><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="plans/jtag-debug-server.html"><strong aria-hidden="true">30.15.</strong> Interactive JTAG debug server</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="plans/python-engine-binary-wheel.html"><strong aria-hidden="true">30.16.</strong> Python engine as a bundled binary wheel</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="plans/netlist-graph-xroots.html"><strong aria-hidden="true">30.17.</strong> netlist-graph xroots query</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="plans/cell-model-ir.html"><strong aria-hidden="true">30.18.</strong> Cell-model IR</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="plans/rtl-onramp-sim-integration.html"><strong aria-hidden="true">30.19.</strong> RTL on-ramp folded into sim/cosim</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="plans/rtl-source-provenance.html"><strong aria-hidden="true">30.20.</strong> RTL-source provenance</a></span></li></ol><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="spikes/opentimer-sky130.html"><strong aria-hidden="true">31.</strong> Spike — OpenTimer on SKY130</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="spikes/amd-laptop-backend.html"><strong aria-hidden="true">32.</strong> Spike — reaching AMD laptops</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="spikes/video-streaming-tap.html"><strong aria-hidden="true">33.</strong> Spike — Live video streaming tap for cosim</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="spikes/architecture-doc-redesign.html"><strong aria-hidden="true">34.</strong> Spike — Architecture documentation redesign</a></span><ol class="section"><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="architecture/index.html"><strong aria-hidden="true">34.1.</strong> Worked example — Architecture map</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="architecture/cosim-runtime.html"><strong aria-hidden="true">34.2.</strong> Worked example — Cosim runtime</a></span></li></ol></li></ol>';
        // Set the current, active page, and reveal it if it's hidden
        let current_page = document.location.href.toString().split('#')[0].split('?')[0];
        if (current_page.endsWith('/')) {
            current_page += 'index.html';
        }
        const links = Array.prototype.slice.call(this.querySelectorAll('a'));
        const l = links.length;
        for (let i = 0; i < l; ++i) {
            const link = links[i];
            const href = link.getAttribute('href');
            if (href && !href.startsWith('#') && !/^(?:[a-z+]+:)?\/\//.test(href)) {
                link.href = path_to_root + href;
            }
            // The 'index' page is supposed to alias the first chapter in the book.
            if (link.href === current_page
                || i === 0
                && path_to_root === ''
                && current_page.endsWith('/index.html')) {
                link.classList.add('active');
                let parent = link.parentElement;
                while (parent) {
                    if (parent.tagName === 'LI' && parent.classList.contains('chapter-item')) {
                        parent.classList.add('expanded');
                    }
                    parent = parent.parentElement;
                }
            }
        }
        // Track and set sidebar scroll position
        this.addEventListener('click', e => {
            if (e.target.tagName === 'A') {
                const clientRect = e.target.getBoundingClientRect();
                const sidebarRect = this.getBoundingClientRect();
                sessionStorage.setItem('sidebar-scroll-offset', clientRect.top - sidebarRect.top);
            }
        }, { passive: true });
        const sidebarScrollOffset = sessionStorage.getItem('sidebar-scroll-offset');
        sessionStorage.removeItem('sidebar-scroll-offset');
        if (sidebarScrollOffset !== null) {
            // preserve sidebar scroll position when navigating via links within sidebar
            const activeSection = this.querySelector('.active');
            if (activeSection) {
                const clientRect = activeSection.getBoundingClientRect();
                const sidebarRect = this.getBoundingClientRect();
                const currentOffset = clientRect.top - sidebarRect.top;
                this.scrollTop += currentOffset - parseFloat(sidebarScrollOffset);
            }
        } else {
            // scroll sidebar to current active section when navigating via
            // 'next/previous chapter' buttons
            const activeSection = document.querySelector('#mdbook-sidebar .active');
            if (activeSection) {
                activeSection.scrollIntoView({ block: 'center' });
            }
        }
        // Toggle buttons
        const sidebarAnchorToggles = document.querySelectorAll('.chapter-fold-toggle');
        function toggleSection(ev) {
            ev.currentTarget.parentElement.parentElement.classList.toggle('expanded');
        }
        Array.from(sidebarAnchorToggles).forEach(el => {
            el.addEventListener('click', toggleSection);
        });
    }
}
window.customElements.define('mdbook-sidebar-scrollbox', MDBookSidebarScrollbox);


// ---------------------------------------------------------------------------
// Support for dynamically adding headers to the sidebar.

(function() {
    // This is used to detect which direction the page has scrolled since the
    // last scroll event.
    let lastKnownScrollPosition = 0;
    // This is the threshold in px from the top of the screen where it will
    // consider a header the "current" header when scrolling down.
    const defaultDownThreshold = 150;
    // Same as defaultDownThreshold, except when scrolling up.
    const defaultUpThreshold = 300;
    // The threshold is a virtual horizontal line on the screen where it
    // considers the "current" header to be above the line. The threshold is
    // modified dynamically to handle headers that are near the bottom of the
    // screen, and to slightly offset the behavior when scrolling up vs down.
    let threshold = defaultDownThreshold;
    // This is used to disable updates while scrolling. This is needed when
    // clicking the header in the sidebar, which triggers a scroll event. It
    // is somewhat finicky to detect when the scroll has finished, so this
    // uses a relatively dumb system of disabling scroll updates for a short
    // time after the click.
    let disableScroll = false;
    // Array of header elements on the page.
    let headers;
    // Array of li elements that are initially collapsed headers in the sidebar.
    // I'm not sure why eslint seems to have a false positive here.
    // eslint-disable-next-line prefer-const
    let headerToggles = [];
    // This is a debugging tool for the threshold which you can enable in the console.
    let thresholdDebug = false;

    // Updates the threshold based on the scroll position.
    function updateThreshold() {
        const scrollTop = window.pageYOffset || document.documentElement.scrollTop;
        const windowHeight = window.innerHeight;
        const documentHeight = document.documentElement.scrollHeight;

        // The number of pixels below the viewport, at most documentHeight.
        // This is used to push the threshold down to the bottom of the page
        // as the user scrolls towards the bottom.
        const pixelsBelow = Math.max(0, documentHeight - (scrollTop + windowHeight));
        // The number of pixels above the viewport, at least defaultDownThreshold.
        // Similar to pixelsBelow, this is used to push the threshold back towards
        // the top when reaching the top of the page.
        const pixelsAbove = Math.max(0, defaultDownThreshold - scrollTop);
        // How much the threshold should be offset once it gets close to the
        // bottom of the page.
        const bottomAdd = Math.max(0, windowHeight - pixelsBelow - defaultDownThreshold);
        let adjustedBottomAdd = bottomAdd;

        // Adjusts bottomAdd for a small document. The calculation above
        // assumes the document is at least twice the windowheight in size. If
        // it is less than that, then bottomAdd needs to be shrunk
        // proportional to the difference in size.
        if (documentHeight < windowHeight * 2) {
            const maxPixelsBelow = documentHeight - windowHeight;
            const t = 1 - pixelsBelow / Math.max(1, maxPixelsBelow);
            const clamp = Math.max(0, Math.min(1, t));
            adjustedBottomAdd *= clamp;
        }

        let scrollingDown = true;
        if (scrollTop < lastKnownScrollPosition) {
            scrollingDown = false;
        }

        if (scrollingDown) {
            // When scrolling down, move the threshold up towards the default
            // downwards threshold position. If near the bottom of the page,
            // adjustedBottomAdd will offset the threshold towards the bottom
            // of the page.
            const amountScrolledDown = scrollTop - lastKnownScrollPosition;
            const adjustedDefault = defaultDownThreshold + adjustedBottomAdd;
            threshold = Math.max(adjustedDefault, threshold - amountScrolledDown);
        } else {
            // When scrolling up, move the threshold down towards the default
            // upwards threshold position. If near the bottom of the page,
            // quickly transition the threshold back up where it normally
            // belongs.
            const amountScrolledUp = lastKnownScrollPosition - scrollTop;
            const adjustedDefault = defaultUpThreshold - pixelsAbove
                + Math.max(0, adjustedBottomAdd - defaultDownThreshold);
            threshold = Math.min(adjustedDefault, threshold + amountScrolledUp);
        }

        if (documentHeight <= windowHeight) {
            threshold = 0;
        }

        if (thresholdDebug) {
            const id = 'mdbook-threshold-debug-data';
            let data = document.getElementById(id);
            if (data === null) {
                data = document.createElement('div');
                data.id = id;
                data.style.cssText = `
                    position: fixed;
                    top: 50px;
                    right: 10px;
                    background-color: 0xeeeeee;
                    z-index: 9999;
                    pointer-events: none;
                `;
                document.body.appendChild(data);
            }
            data.innerHTML = `
                <table>
                  <tr><td>documentHeight</td><td>${documentHeight.toFixed(1)}</td></tr>
                  <tr><td>windowHeight</td><td>${windowHeight.toFixed(1)}</td></tr>
                  <tr><td>scrollTop</td><td>${scrollTop.toFixed(1)}</td></tr>
                  <tr><td>pixelsAbove</td><td>${pixelsAbove.toFixed(1)}</td></tr>
                  <tr><td>pixelsBelow</td><td>${pixelsBelow.toFixed(1)}</td></tr>
                  <tr><td>bottomAdd</td><td>${bottomAdd.toFixed(1)}</td></tr>
                  <tr><td>adjustedBottomAdd</td><td>${adjustedBottomAdd.toFixed(1)}</td></tr>
                  <tr><td>scrollingDown</td><td>${scrollingDown}</td></tr>
                  <tr><td>threshold</td><td>${threshold.toFixed(1)}</td></tr>
                </table>
            `;
            drawDebugLine();
        }

        lastKnownScrollPosition = scrollTop;
    }

    function drawDebugLine() {
        if (!document.body) {
            return;
        }
        const id = 'mdbook-threshold-debug-line';
        const existingLine = document.getElementById(id);
        if (existingLine) {
            existingLine.remove();
        }
        const line = document.createElement('div');
        line.id = id;
        line.style.cssText = `
            position: fixed;
            top: ${threshold}px;
            left: 0;
            width: 100vw;
            height: 2px;
            background-color: red;
            z-index: 9999;
            pointer-events: none;
        `;
        document.body.appendChild(line);
    }

    function mdbookEnableThresholdDebug() {
        thresholdDebug = true;
        updateThreshold();
        drawDebugLine();
    }

    window.mdbookEnableThresholdDebug = mdbookEnableThresholdDebug;

    // Updates which headers in the sidebar should be expanded. If the current
    // header is inside a collapsed group, then it, and all its parents should
    // be expanded.
    function updateHeaderExpanded(currentA) {
        // Add expanded to all header-item li ancestors.
        let current = currentA.parentElement;
        while (current) {
            if (current.tagName === 'LI' && current.classList.contains('header-item')) {
                current.classList.add('expanded');
            }
            current = current.parentElement;
        }
    }

    // Updates which header is marked as the "current" header in the sidebar.
    // This is done with a virtual Y threshold, where headers at or below
    // that line will be considered the current one.
    function updateCurrentHeader() {
        if (!headers || !headers.length) {
            return;
        }

        // Reset the classes, which will be rebuilt below.
        const els = document.getElementsByClassName('current-header');
        for (const el of els) {
            el.classList.remove('current-header');
        }
        for (const toggle of headerToggles) {
            toggle.classList.remove('expanded');
        }

        // Find the last header that is above the threshold.
        let lastHeader = null;
        for (const header of headers) {
            const rect = header.getBoundingClientRect();
            if (rect.top <= threshold) {
                lastHeader = header;
            } else {
                break;
            }
        }
        if (lastHeader === null) {
            lastHeader = headers[0];
            const rect = lastHeader.getBoundingClientRect();
            const windowHeight = window.innerHeight;
            if (rect.top >= windowHeight) {
                return;
            }
        }

        // Get the anchor in the summary.
        const href = '#' + lastHeader.id;
        const a = [...document.querySelectorAll('.header-in-summary')]
            .find(element => element.getAttribute('href') === href);
        if (!a) {
            return;
        }

        a.classList.add('current-header');

        updateHeaderExpanded(a);
    }

    // Updates which header is "current" based on the threshold line.
    function reloadCurrentHeader() {
        if (disableScroll) {
            return;
        }
        updateThreshold();
        updateCurrentHeader();
    }


    // When clicking on a header in the sidebar, this adjusts the threshold so
    // that it is located next to the header. This is so that header becomes
    // "current".
    function headerThresholdClick(event) {
        // See disableScroll description why this is done.
        disableScroll = true;
        setTimeout(() => {
            disableScroll = false;
        }, 100);
        // requestAnimationFrame is used to delay the update of the "current"
        // header until after the scroll is done, and the header is in the new
        // position.
        requestAnimationFrame(() => {
            requestAnimationFrame(() => {
                // Closest is needed because if it has child elements like <code>.
                const a = event.target.closest('a');
                const href = a.getAttribute('href');
                const targetId = href.substring(1);
                const targetElement = document.getElementById(targetId);
                if (targetElement) {
                    threshold = targetElement.getBoundingClientRect().bottom;
                    updateCurrentHeader();
                }
            });
        });
    }

    // Takes the nodes from the given head and copies them over to the
    // destination, along with some filtering.
    function filterHeader(source, dest) {
        const clone = source.cloneNode(true);
        clone.querySelectorAll('mark').forEach(mark => {
            mark.replaceWith(...mark.childNodes);
        });
        dest.append(...clone.childNodes);
    }

    // Scans page for headers and adds them to the sidebar.
    document.addEventListener('DOMContentLoaded', function() {
        const activeSection = document.querySelector('#mdbook-sidebar .active');
        if (activeSection === null) {
            return;
        }

        const main = document.getElementsByTagName('main')[0];
        headers = Array.from(main.querySelectorAll('h2, h3, h4, h5, h6'))
            .filter(h => h.id !== '' && h.children.length && h.children[0].tagName === 'A');

        if (headers.length === 0) {
            return;
        }

        // Build a tree of headers in the sidebar.

        const stack = [];

        const firstLevel = parseInt(headers[0].tagName.charAt(1));
        for (let i = 1; i < firstLevel; i++) {
            const ol = document.createElement('ol');
            ol.classList.add('section');
            if (stack.length > 0) {
                stack[stack.length - 1].ol.appendChild(ol);
            }
            stack.push({level: i + 1, ol: ol});
        }

        // The level where it will start folding deeply nested headers.
        const foldLevel = 3;

        for (let i = 0; i < headers.length; i++) {
            const header = headers[i];
            const level = parseInt(header.tagName.charAt(1));

            const currentLevel = stack[stack.length - 1].level;
            if (level > currentLevel) {
                // Begin nesting to this level.
                for (let nextLevel = currentLevel + 1; nextLevel <= level; nextLevel++) {
                    const ol = document.createElement('ol');
                    ol.classList.add('section');
                    const last = stack[stack.length - 1];
                    const lastChild = last.ol.lastChild;
                    // Handle the case where jumping more than one nesting
                    // level, which doesn't have a list item to place this new
                    // list inside of.
                    if (lastChild) {
                        lastChild.appendChild(ol);
                    } else {
                        last.ol.appendChild(ol);
                    }
                    stack.push({level: nextLevel, ol: ol});
                }
            } else if (level < currentLevel) {
                while (stack.length > 1 && stack[stack.length - 1].level > level) {
                    stack.pop();
                }
            }

            const li = document.createElement('li');
            li.classList.add('header-item');
            li.classList.add('expanded');
            if (level < foldLevel) {
                li.classList.add('expanded');
            }
            const span = document.createElement('span');
            span.classList.add('chapter-link-wrapper');
            const a = document.createElement('a');
            span.appendChild(a);
            a.href = '#' + header.id;
            a.classList.add('header-in-summary');
            filterHeader(header.children[0], a);
            a.addEventListener('click', headerThresholdClick);
            const nextHeader = headers[i + 1];
            if (nextHeader !== undefined) {
                const nextLevel = parseInt(nextHeader.tagName.charAt(1));
                if (nextLevel > level && level >= foldLevel) {
                    const toggle = document.createElement('a');
                    toggle.classList.add('chapter-fold-toggle');
                    toggle.classList.add('header-toggle');
                    toggle.addEventListener('click', () => {
                        li.classList.toggle('expanded');
                    });
                    const toggleDiv = document.createElement('div');
                    toggleDiv.textContent = '❱';
                    toggle.appendChild(toggleDiv);
                    span.appendChild(toggle);
                    headerToggles.push(li);
                }
            }
            li.appendChild(span);

            const currentParent = stack[stack.length - 1];
            currentParent.ol.appendChild(li);
        }

        const onThisPage = document.createElement('div');
        onThisPage.classList.add('on-this-page');
        onThisPage.append(stack[0].ol);
        const activeItemSpan = activeSection.parentElement;
        activeItemSpan.after(onThisPage);
    });

    document.addEventListener('DOMContentLoaded', reloadCurrentHeader);
    document.addEventListener('scroll', reloadCurrentHeader, { passive: true });
})();


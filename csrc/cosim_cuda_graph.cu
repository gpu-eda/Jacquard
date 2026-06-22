// SPDX-License-Identifier: Apache-2.0
//
// CUDA Graphs capture/replay helpers for the cosim per-edge launch chain.
//
// The cosim CUDA backend issues a long sequence of tiny, strictly-dependent
// kernel launches per scheduler edge (state_prep -> apply_flash_din ->
// simulate_stage x N -> flash_model_step -> io_step -> snapshot). ncu on the T4
// (#122) showed cosim_simulate_stage is latency / under-utilisation bound
// (Waves/SM 0.80, SM throughput 7.9%): the grid underfills the GPU and the
// kernel chain is dominated by per-launch overhead, not work. Capturing the
// repeating chain into a cudaGraph and replaying it removes per-launch host
// submission cost and lets the driver schedule the dependent chain back to back.
//
// The cosim launchers in kernel_v1.cu issue on cudaStreamPerThread explicitly
// (each `<<<g, b, 0, cudaStreamPerThread>>>` plus the `cudaMemcpyAsync(...,
// cudaStreamPerThread)` snapshot), so we capture exactly that stream here.
//
// These helpers take no UVec arguments, so they are intentionally NOT run
// through ucc::bindgen (whose convention maps pointer args to UVecs and appends
// a Device arg). The Rust side declares them via a hand-written `extern "C"`
// block (src/sim/cosim/cuda.rs).

#include <cstdio>
#include <cstdlib>
#include <cuda_runtime.h>

// Mirrors the macro in csrc/kernel_v1.cu — extract to a shared csrc header if a
// third CUDA TU ever needs it.
#define checkCudaErrors(call)                                 \
  do {                                                        \
    cudaError_t err = call;                                   \
    if (err != cudaSuccess) {                                 \
      printf("CUDA error at %s %d: %s\n", __FILE__, __LINE__, \
             cudaGetErrorString(err));                        \
      exit(EXIT_FAILURE);                                     \
    }                                                         \
  } while (0)

extern "C" {

// Begin capturing cudaStreamPerThread. Subsequent cosim launches (which target
// that stream explicitly) are recorded into the graph instead of executed.
void cosim_graph_begin_capture() {
  checkCudaErrors(cudaStreamBeginCapture(
      cudaStreamPerThread, cudaStreamCaptureModeThreadLocal));
}

// End capture, instantiate the captured graph into an executable graph, and
// return it as an opaque handle. The intermediate cudaGraph_t is destroyed; the
// caller owns the returned cudaGraphExec_t and frees it via
// cosim_graph_exec_destroy.
void *cosim_graph_end_capture() {
  cudaGraph_t graph = nullptr;
  checkCudaErrors(cudaStreamEndCapture(cudaStreamPerThread, &graph));
  cudaGraphExec_t exec = nullptr;
  // CUDA 12 three-argument form: (pGraphExec, graph, flags).
  checkCudaErrors(cudaGraphInstantiate(&exec, graph, 0));
  checkCudaErrors(cudaGraphDestroy(graph));
  return (void *)exec;
}

// Enqueue a previously-instantiated graph on cudaStreamPerThread. The caller
// (run_edges) issues a single end-of-batch device synchronize, so this does not
// block here.
void cosim_graph_launch(void *exec) {
  checkCudaErrors(cudaGraphLaunch((cudaGraphExec_t)exec, cudaStreamPerThread));
}

void cosim_graph_exec_destroy(void *exec) {
  if (exec) checkCudaErrors(cudaGraphExecDestroy((cudaGraphExec_t)exec));
}

}  // extern "C"

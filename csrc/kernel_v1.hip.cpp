// SPDX-License-Identifier: Apache-2.0
// HIP kernel launch wrapper for AMD GPUs.
// Mirrors kernel_v1.cu — uses hipLaunchCooperativeKernel for grid-wide sync.

#include <hip/hip_runtime.h>
#include "kernel_v1_impl.cuh"

#define checkHipErrors(call)                                    \
  do {                                                          \
    hipError_t err = call;                                      \
    if (err != hipSuccess) {                                    \
      printf("HIP error at %s %d: %s\n", __FILE__, __LINE__,   \
             hipGetErrorString(err));                            \
      exit(EXIT_FAILURE);                                       \
    }                                                           \
  } while (0)

// One-time warp size validation — RDNA uses wave32, matching CUDA.
// Called once before the first kernel launch.
static void validate_warp_size() {
  static bool checked = false;
  if (checked) return;
  checked = true;
  int warp_size = 0;
  hipDeviceGetAttribute(&warp_size, hipDeviceAttributeWarpSize, 0);
  if (warp_size != 32) {
    printf("ERROR: Jacquard requires warpSize==32 (RDNA), but this GPU reports %d.\n"
           "CDNA / GCN GPUs (wave64) are not supported.\n", warp_size);
    exit(EXIT_FAILURE);
  }
}

// Original function without timing support (backward compatible).
extern "C"
void simulate_v1_noninteractive_simple_scan_hip(
  usize num_blocks,
  usize num_major_stages,
  const usize *blocks_start,
  const u32 *blocks_data,
  u32 *sram_data,
  u32 *sram_xmask,
  usize num_cycles,
  usize state_size,
  u32 *states_noninteractive,
  i32 arrival_state_offset
  )
{
  validate_warp_size();

  const u32 *timing_constraints = nullptr;
  EventBuffer *event_buffer = nullptr;
  void *arg_ptrs[12] = {
    (void *)&num_blocks, (void *)&num_major_stages,
    (void *)&blocks_start, (void *)&blocks_data,
    (void *)&sram_data, (void *)&sram_xmask,
    (void *)&num_cycles, (void *)&state_size,
    (void *)&states_noninteractive,
    (void *)&timing_constraints, (void *)&event_buffer,
    (void *)&arrival_state_offset
  };
  checkHipErrors(hipLaunchCooperativeKernel(
    (void *)simulate_v1_noninteractive_simple_scan,
    dim3(num_blocks), dim3(256),
    arg_ptrs, 0, (hipStream_t)0
    ));
}

// ── cosim launchers (non-cooperative; #105 Phase 2) ──────────────────────────
// Ordinary launches (no cooperative grid.sync); the host loops major stages.

extern "C"
void cosim_state_prep_hip(
  u32 *states,
  u32 state_size,
  u32 num_ops,
  u32 xmask_state_offset,
  const u32 *ops
  )
{
  validate_warp_size();
  hipLaunchKernelGGL(cosim_state_prep, dim3(1), dim3(256), 0, (hipStream_t)0,
                     states, state_size, num_ops, xmask_state_offset, ops);
  checkHipErrors(hipGetLastError());
}

extern "C"
void cosim_simulate_stage_hip(
  usize num_blocks,
  const usize *blocks_start,
  const u32 *blocks_data,
  u32 *sram_data,
  u32 *sram_xmask,
  usize state_size,
  u32 *states,
  usize current_stage,
  const u32 *timing_constraints,
  u8 *event_buffer,
  i32 arrival_state_offset
  )
{
  validate_warp_size();
  hipLaunchKernelGGL(cosim_simulate_stage, dim3(num_blocks), dim3(256), 0, (hipStream_t)0,
                     num_blocks, blocks_start, blocks_data, sram_data, sram_xmask,
                     state_size, states, current_stage,
                     timing_constraints, (EventBuffer *)event_buffer, arrival_state_offset);
  checkHipErrors(hipGetLastError());
}

// Extended function with timing constraints and event buffer support.
extern "C"
void simulate_v1_noninteractive_timed_hip(
  usize num_blocks,
  usize num_major_stages,
  const usize *blocks_start,
  const u32 *blocks_data,
  u32 *sram_data,
  u32 *sram_xmask,
  usize num_cycles,
  usize state_size,
  u32 *states_noninteractive,
  const u32 *timing_constraints,
  u8 *event_buffer,
  i32 arrival_state_offset
  )
{
  validate_warp_size();

  void *arg_ptrs[12] = {
    (void *)&num_blocks, (void *)&num_major_stages,
    (void *)&blocks_start, (void *)&blocks_data,
    (void *)&sram_data, (void *)&sram_xmask,
    (void *)&num_cycles, (void *)&state_size,
    (void *)&states_noninteractive,
    (void *)&timing_constraints, (void *)&event_buffer,
    (void *)&arrival_state_offset
  };
  checkHipErrors(hipLaunchCooperativeKernel(
    (void *)simulate_v1_noninteractive_simple_scan,
    dim3(num_blocks), dim3(256),
    arg_ptrs, 0, (hipStream_t)0
    ));
}

// gpu_io_step launcher (Stage B): UART + bus-trace capture for one edge.
// Non-cooperative single-block launch (thread-0 work), mirrors kernel_v1.cu.
// IO struct buffers cross FFI as untyped u8 (event_buffer pattern); cast here.
extern "C"
void gpu_io_step_hip(
  u32 *states,
  u8 *uart_state,
  const u8 *uart_params,
  u8 *uart_channel,
  u8 *wb_channel,
  const u8 *wb_params,
  u8 *bus_channel,
  const u8 *bus_params
  )
{
  hipLaunchKernelGGL(gpu_io_step, dim3(1), dim3(256), 0, (hipStream_t)0,
                     states,
                     (UartDecoderState *)uart_state,
                     (const UartParams *)uart_params,
                     (UartChannel *)uart_channel,
                     (WbTraceChannel *)wb_channel,
                     (const WbTraceParams *)wb_params,
                     (BusTraceChannel *)bus_channel,
                     (const BusTraceParamsAll *)bus_params);
  checkHipErrors(hipGetLastError());
}

// gpu_apply_flash_din launcher (Stage C): inject FlashState.d_i → input-state
// MISO bits. Single-block thread-0 kernel; runs after state_prep, before
// simulate. FlashState/FlashDinParams cross FFI as untyped u8 (event_buffer
// pattern); cast here. Mirrors kernel_v1.cu gpu_apply_flash_din_cuda.
extern "C"
void gpu_apply_flash_din_hip(
  u32 *states,
  const u8 *flash_state,
  const u8 *flash_din_params
  )
{
  hipLaunchKernelGGL(gpu_apply_flash_din, dim3(1), dim3(256), 0, (hipStream_t)0,
                     states,
                     (const FlashState *)flash_state,
                     (const FlashDinParams *)flash_din_params);
  checkHipErrors(hipGetLastError());
}

// gpu_flash_model_step launcher (Stage C): dual-step SPI/QSPI FSM over the output
// state slot; updates persistent FlashState. Single-block thread-0 kernel; runs
// after simulate. FlashState (mutable) + FlashModelParams + 16 MiB firmware cross
// FFI as untyped u8; cast here. Mirrors kernel_v1.cu gpu_flash_model_step_cuda.
extern "C"
void gpu_flash_model_step_hip(
  u32 *states,
  u8 *flash_state,
  const u8 *flash_model_params,
  const u8 *flash_data
  )
{
  hipLaunchKernelGGL(gpu_flash_model_step, dim3(1), dim3(256), 0, (hipStream_t)0,
                     states,
                     (FlashState *)flash_state,
                     (const FlashModelParams *)flash_model_params,
                     flash_data);
  checkHipErrors(hipGetLastError());
}

// cosim_snapshot launcher (Stage B): device→device copy of the 2-slot state
// into ring slot `edge_offset`. Default-stream ordered with the kernels; host
// syncs once at end-of-batch. Mirrors kernel_v1.cu cosim_snapshot_cuda.
extern "C"
void cosim_snapshot_hip(
  const u32 *states,
  u32 *ring,
  usize two_slot_words,
  usize edge_offset
  )
{
  checkHipErrors(hipMemcpyAsync(
    ring + edge_offset * two_slot_words,
    states,
    two_slot_words * sizeof(u32),
    hipMemcpyDeviceToDevice,
    (hipStream_t)0));
}

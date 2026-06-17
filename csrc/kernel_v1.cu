// SPDX-FileCopyrightText: Copyright (c) 2024 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

#include "kernel_v1_impl.cuh"

#define checkCudaErrors(call)                                 \
  do {                                                        \
    cudaError_t err = call;                                   \
    if (err != cudaSuccess) {                                 \
      printf("CUDA error at %s %d: %s\n", __FILE__, __LINE__, \
             cudaGetErrorString(err));                        \
      exit(EXIT_FAILURE);                                     \
    }                                                         \
  } while (0)

// Original function without timing support (backward compatible).
extern "C"
void simulate_v1_noninteractive_simple_scan_cuda(
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
  checkCudaErrors(cudaLaunchCooperativeKernel(
    (void *)simulate_v1_noninteractive_simple_scan, num_blocks, 256,
    arg_ptrs, 0, (cudaStream_t)0
    ));
}

// Extended function with timing constraints and event buffer support.
extern "C"
void simulate_v1_noninteractive_timed_cuda(
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
  void *arg_ptrs[12] = {
    (void *)&num_blocks, (void *)&num_major_stages,
    (void *)&blocks_start, (void *)&blocks_data,
    (void *)&sram_data, (void *)&sram_xmask,
    (void *)&num_cycles, (void *)&state_size,
    (void *)&states_noninteractive,
    (void *)&timing_constraints, (void *)&event_buffer,
    (void *)&arrival_state_offset
  };
  checkCudaErrors(cudaLaunchCooperativeKernel(
    (void *)simulate_v1_noninteractive_simple_scan, num_blocks, 256,
    arg_ptrs, 0, (cudaStream_t)0
    ));
}

// ── cosim launchers (non-cooperative; #105 Phase 2) ──────────────────────────
// Ordinary launches (no cooperative grid.sync); the host loops major stages.

extern "C"
void cosim_state_prep_cuda(
  u32 *states,
  u32 state_size,
  u32 num_ops,
  u32 xmask_state_offset,
  const u32 *ops
  )
{
  cosim_state_prep<<<1, 256>>>(states, state_size, num_ops, xmask_state_offset, ops);
  checkCudaErrors(cudaGetLastError());
}

extern "C"
void cosim_simulate_stage_cuda(
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
  cosim_simulate_stage<<<(unsigned int)num_blocks, 256>>>(
    num_blocks, blocks_start, blocks_data, sram_data, sram_xmask,
    state_size, states, current_stage,
    timing_constraints, (EventBuffer *)event_buffer, arrival_state_offset);
  checkCudaErrors(cudaGetLastError());
}

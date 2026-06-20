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

// gpu_io_step launcher (Stage B): UART + bus-trace capture for one edge. Single
// block, thread-0 work like the Metal encode_io_step. Default-stream ordered
// after this edge's cosim_simulate_stage launches.
// IO struct buffers cross FFI as untyped u8 (the proven event_buffer pattern):
// ulib `UVec<T>` requires `T: UniversalCopy`, which the IO structs are not, so
// the Rust side passes `UVec<u8>` byte buffers and the launcher casts to the
// struct types here. `states` stays u32 (the design-state UVec<u32>).
extern "C"
void gpu_io_step_cuda(
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
  gpu_io_step<<<1, 256>>>(
    states,
    (UartDecoderState *)uart_state,
    (const UartParams *)uart_params,
    (UartChannel *)uart_channel,
    (WbTraceChannel *)wb_channel,
    (const WbTraceParams *)wb_params,
    (BusTraceChannel *)bus_channel,
    (const BusTraceParamsAll *)bus_params);
  checkCudaErrors(cudaGetLastError());
}

// gpu_apply_flash_din launcher (Stage C): inject FlashState.d_i → input-state
// MISO bits. Single-block thread-0 kernel; runs after this edge's state_prep,
// before simulate. FlashState/FlashDinParams cross FFI as untyped u8 (the
// event_buffer pattern; ulib UVec<u8>) and are cast here. `states` stays u32.
extern "C"
void gpu_apply_flash_din_cuda(
  u32 *states,
  const u8 *flash_state,
  const u8 *flash_din_params
  )
{
  gpu_apply_flash_din<<<1, 256>>>(
    states,
    (const FlashState *)flash_state,
    (const FlashDinParams *)flash_din_params);
  checkCudaErrors(cudaGetLastError());
}

// gpu_flash_model_step launcher (Stage C): dual-step SPI/QSPI FSM over the output
// state slot; updates persistent FlashState. Single-block thread-0 kernel; runs
// after this edge's simulate stages. FlashState (mutable) + FlashModelParams +
// 16 MiB firmware cross FFI as untyped u8 and are cast here.
extern "C"
void gpu_flash_model_step_cuda(
  u32 *states,
  u8 *flash_state,
  const u8 *flash_model_params,
  const u8 *flash_data
  )
{
  gpu_flash_model_step<<<1, 256>>>(
    states,
    (FlashState *)flash_state,
    (const FlashModelParams *)flash_model_params,
    flash_data);
  checkCudaErrors(cudaGetLastError());
}

// cosim_snapshot launcher (Stage B): device→device copy of the 2-slot state
// (`two_slot_words` = 2*state_size u32s) into ring slot `edge_offset`, so each
// batched edge's [input|output] snapshot survives the next edge overwriting
// `states`. The CUDA analog of Metal's per-edge blit (cosim/metal.rs blit copy).
// Default-stream (stream 0) ordered with the kernel launches; the host syncs
// once at end-of-batch.
extern "C"
void cosim_snapshot_cuda(
  const u32 *states,
  u32 *ring,
  usize two_slot_words,
  usize edge_offset
  )
{
  checkCudaErrors(cudaMemcpyAsync(
    ring + edge_offset * two_slot_words,
    states,
    two_slot_words * sizeof(u32),
    cudaMemcpyDeviceToDevice,
    0));
}

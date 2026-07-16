// SPDX-FileCopyrightText: Copyright (c) 2024 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

#include <crates/ulib/includes.hpp>
#include <cstdio>

#if defined(__HIP_PLATFORM_AMD__)
#include <hip/hip_runtime.h>
#include <hip/hip_cooperative_groups.h>
#else
#include <cooperative_groups.h>
#endif

// Lane mask for `__shfl_*_sync`. CUDA takes a 32-bit mask (a warp is 32 lanes).
// HIP-on-AMD takes a **64-bit** mask, because an AMD wave can be 64 lanes, and
// static_asserts `sizeof(mask) == 8` — passing an `unsigned` there is a hard
// compile error:
//
//   amd_warp_sync_functions.h:297:62: error: static assertion failed due to
//   requirement 'sizeof(unsigned int) == 8': The mask must be a 64-bit integer.
//
// Jacquard only runs on wave32 (kernel_v1.hip.cpp rejects wave64 at startup),
// so lanes 32..63 never exist and the extra width is inert: widening a 32-bit
// mask zero-extends, leaving those lanes unset, which is exactly right. Masks
// here are sometimes partial (`0xffffffff >> n` for a ragged tail), so this is a
// *type*, not a constant — the value must survive the widening.
#if defined(__HIP_PLATFORM_AMD__)
typedef unsigned long long lane_mask_t;
#else
typedef unsigned lane_mask_t;
#endif
#define LANE_MASK_ALL ((lane_mask_t)0xffffffffu)

#include "event_buffer.h"

struct alignas(8) VectorRead2 {
  u32 c1, c2;

  __device__ __forceinline__ void read(const VectorRead2 *t) {
    *this = *t;
  }
};

struct alignas(16) VectorRead4 {
  u32 c1, c2, c3, c4;

  __device__ __forceinline__ void read(const VectorRead4 *t) {
    *this = *t;
  }
};

__device__ void simulate_block_v1(
  const u32 *__restrict__ script,
  usize script_size,
  const u32 *__restrict__ input_state,
  u32 *__restrict__ output_state,
  u32 *__restrict__ sram_data,
  u32 *__restrict__ sram_xmask,  // X-mask shadow for SRAM (nullptr if xprop disabled)
  u32 *__restrict__ shared_metadata,
  u32 *__restrict__ shared_writeouts,
  u32 *__restrict__ shared_state,
  u32 *__restrict__ shared_writeouts_x,  // X-mask sideband for writeouts
  u32 *__restrict__ shared_state_x,      // X-mask sideband for state
  // Arrival times in raw picoseconds (u16), max representable 65,535ps.
  // Gate delays are stored as u16 in the script padding slots.
  // No quantization needed — 512 bytes of shared memory per block is negligible.
  u16 *__restrict__ shared_arrival,
  // Writeout arrival times captured during writeout hook
  u16 *__restrict__ shared_writeout_arrival,
  // Timing constraint buffer: one u32 per state word, [setup_ps:16][hold_ps:16]
  const u32 *__restrict__ timing_constraints,
  u32 clock_period_ps,
  EventBuffer *__restrict__ event_buffer,
  u32 cycle_i,
  i32 arrival_state_offset  // offset in output_state for arrival data (0 = disabled)
  )
{
  int script_pi = 0;
  while(true) {
    VectorRead2 t2_1, t2_2;
    VectorRead4 t4_1, t4_2, t4_3, t4_4, t4_5;
    shared_metadata[threadIdx.x] = script[script_pi + threadIdx.x];
    script_pi += 256;
    t2_1.read(((const VectorRead2 *)(script + script_pi)) + threadIdx.x);
    __syncthreads();
    int num_stages = shared_metadata[0];
    if(!num_stages) {
      break;
    }
    int is_last_part = shared_metadata[1];
    int num_ios = shared_metadata[2];
    int io_offset = shared_metadata[3];
    int num_srams = shared_metadata[4];
    int sram_offset = shared_metadata[5];
    int num_global_read_rounds = shared_metadata[6];
    int num_output_duplicates = shared_metadata[7];
    bool is_x_capable = (shared_metadata[8] != 0);
    int xmask_state_offset = (int)shared_metadata[9];
    u32 writeout_hook_i = shared_metadata[128 + threadIdx.x / 2];
    if(threadIdx.x % 2 == 0) {
      writeout_hook_i = writeout_hook_i & ((1 << 16) - 1);
    }
    else {
      writeout_hook_i = writeout_hook_i >> 16;
    }

    t4_1.read((const VectorRead4 *)(script + script_pi + 256 * 2 * num_global_read_rounds) + threadIdx.x);
    t4_2.read((const VectorRead4 *)(script + script_pi + 256 * 2 * num_global_read_rounds + 256 * 4) + threadIdx.x);
    t4_3.read((const VectorRead4 *)(script + script_pi + 256 * 2 * num_global_read_rounds + 256 * 4 * 2) + threadIdx.x);
    t4_4.read((const VectorRead4 *)(script + script_pi + 256 * 2 * num_global_read_rounds + 256 * 4 * 3) + threadIdx.x);
    t4_5.read((const VectorRead4 *)(script + script_pi + 256 * 2 * num_global_read_rounds + 256 * 4 * 4) + threadIdx.x);
    u32 t_global_rd_state = 0;
    u32 t_global_rd_state_x = 0;
    for(int gr_i = 0; gr_i < num_global_read_rounds; gr_i += 2) {
      u32 idx = t2_1.c1;
      u32 mask = t2_1.c2;
      script_pi += 256 * 2;
      t2_2.read(((const VectorRead2 *)(script + script_pi)) + threadIdx.x);
      if(mask) {
        const u32 *real_input_array;
        const u32 *real_xmask_array;
        // Bit 31 of idx flags a read of this cycle's inter-stage intermediates
        // (output slot) rather than previous-cycle state; flatten.rs sets it,
        // cpu_reference.rs decodes it the same way. Clear it from the index —
        // do NOT bias the base pointer by -2^31 instead: that forms an
        // out-of-bounds pointer (UB) and got the sign wrong, putting the access
        // 2^32 words out of bounds. nvcc narrowed it away; ROCm faulted (#203).
        if(idx >> 31) {
          real_input_array = output_state;
          real_xmask_array = output_state + xmask_state_offset;
          idx ^= (1u << 31);
        } else {
          real_input_array = input_state;
          real_xmask_array = input_state + xmask_state_offset;
        }
        u32 value = real_input_array[idx];
        u32 xmval = is_x_capable ? real_xmask_array[idx] : 0;
        u32 m = mask;
        while(m) {
          t_global_rd_state <<= 1;
          t_global_rd_state_x <<= 1;
          u32 lowbit = m & -m;
          if(value & lowbit) t_global_rd_state |= 1;
          if(xmval & lowbit) t_global_rd_state_x |= 1;
          m ^= lowbit;
        }
      }

      if(gr_i + 1 >= num_global_read_rounds) break;
      idx = t2_2.c1;
      mask = t2_2.c2;
      script_pi += 256 * 2;
      t2_1.read(((const VectorRead2 *)(script + script_pi)) + threadIdx.x);
      if(mask) {
        const u32 *real_input_array;
        const u32 *real_xmask_array;
        // Staged-IO flag decode; see the twin site above.
        if(idx >> 31) {
          real_input_array = output_state;
          real_xmask_array = output_state + xmask_state_offset;
          idx ^= (1u << 31);
        } else {
          real_input_array = input_state;
          real_xmask_array = input_state + xmask_state_offset;
        }
        u32 value = real_input_array[idx];
        u32 xmval = is_x_capable ? real_xmask_array[idx] : 0;
        u32 m = mask;
        while(m) {
          t_global_rd_state <<= 1;
          t_global_rd_state_x <<= 1;
          u32 lowbit = m & -m;
          if(value & lowbit) t_global_rd_state |= 1;
          if(xmval & lowbit) t_global_rd_state_x |= 1;
          m ^= lowbit;
        }
      }
    }
    shared_state[threadIdx.x] = t_global_rd_state;
    shared_state_x[threadIdx.x] = t_global_rd_state_x;
    // Initialize arrival times to 0 (inputs have zero arrival)
    shared_arrival[threadIdx.x] = 0;
    __syncthreads();

    for(int bs_i = 0; bs_i < num_stages; ++bs_i) {
      u32 hier_input = 0, hier_input_x = 0;
      u32 hier_flag_xora = 0, hier_flag_xorb = 0, hier_flag_orb = 0;
#define GEMV1_SHUF_INPUT_K(k_outer, k_inner, t_shuffle) {           \
        u32 k = k_outer * 4 + k_inner;                              \
        u32 t_shuffle_1_idx = t_shuffle & ((1 << 16) - 1);          \
        u32 t_shuffle_2_idx = t_shuffle >> 16;                      \
                                                                    \
        hier_input |= (shared_state[t_shuffle_1_idx >> 5] >>        \
                       (t_shuffle_1_idx & 31) & 1) << (k * 2);      \
        hier_input |= (shared_state[t_shuffle_2_idx >> 5] >>        \
                       (t_shuffle_2_idx & 31) & 1) << (k * 2 + 1);  \
        if(is_x_capable) {                                          \
          hier_input_x |= (shared_state_x[t_shuffle_1_idx >> 5] >>  \
                           (t_shuffle_1_idx & 31) & 1) << (k * 2);  \
          hier_input_x |= (shared_state_x[t_shuffle_2_idx >> 5] >>  \
                           (t_shuffle_2_idx & 31) & 1) << (k * 2 + 1); \
        }                                                           \
      }
#define GEMV1_SHUF_INPUT_K_4(k_outer, t_shuffle) {    \
        GEMV1_SHUF_INPUT_K(k_outer, 0, t_shuffle.c1); \
        GEMV1_SHUF_INPUT_K(k_outer, 1, t_shuffle.c2); \
        GEMV1_SHUF_INPUT_K(k_outer, 2, t_shuffle.c3); \
        GEMV1_SHUF_INPUT_K(k_outer, 3, t_shuffle.c4); \
      }
      script_pi += 256 * 4 * 5;
      GEMV1_SHUF_INPUT_K_4(0, t4_1);
      t4_1.read(((const VectorRead4 *)(script + script_pi)) + threadIdx.x);
      GEMV1_SHUF_INPUT_K_4(1, t4_2);
      t4_2.read(((const VectorRead4 *)(script + script_pi + 256 * 4)) + threadIdx.x);
      GEMV1_SHUF_INPUT_K_4(2, t4_3);
      t4_3.read(((const VectorRead4 *)(script + script_pi + 256 * 4 * 2)) + threadIdx.x);
      GEMV1_SHUF_INPUT_K_4(3, t4_4);
      t4_4.read(((const VectorRead4 *)(script + script_pi + 256 * 4 * 3)) + threadIdx.x);
#undef GEMV1_SHUF_INPUT_K
#undef GEMV1_SHUF_INPUT_K_4
      hier_flag_xora = t4_5.c1;
      hier_flag_xorb = t4_5.c2;
      hier_flag_orb = t4_5.c3;
      // Extract per-thread-position gate delay from padding slot (u16 raw picoseconds)
      u16 gate_delay = (u16)(t4_5.c4 & 0xFFFF);
      t4_5.read(((const VectorRead4 *)(script + script_pi + 256 * 4 * 4)) + threadIdx.x);

      __syncthreads();
      shared_state[threadIdx.x] = hier_input;
      shared_state_x[threadIdx.x] = hier_input_x;
      shared_arrival[threadIdx.x] = 0;  // Reset arrival for shuffle inputs
      __syncthreads();

      // hier[0]: threads 128-255 compute AND gates + track arrivals
      if(threadIdx.x >= 128) {
        u32 hier_input_a = shared_state[threadIdx.x - 128];
        u32 hier_input_b = hier_input;
        u32 a_eff = hier_input_a ^ hier_flag_xora;
        u32 b_eff = (hier_input_b ^ hier_flag_xorb) | hier_flag_orb;
        u32 ret = a_eff & b_eff;
        shared_state[threadIdx.x] = ret;

        if(is_x_capable) {
          u32 a_x = shared_state_x[threadIdx.x - 128];
          u32 b_x = hier_input_x;
          u32 b_eff_x = b_x & ~hier_flag_orb;
          u32 ret_x = (a_x | b_eff_x) & (a_eff | a_x) & (b_eff | b_eff_x);
          shared_state_x[threadIdx.x] = ret_x;
        }

        // Arrival tracking: max(input_a, input_b) + gate_delay
        // Delay added even for pass-throughs (physical cells with accumulated delays)
        u16 arr_a = (u16)shared_arrival[threadIdx.x - 128];
        u16 arr_b = (u16)shared_arrival[threadIdx.x];
        bool is_pass = (hier_flag_orb == 0xFFFFFFFF);
        u16 base_arr = is_pass ? arr_a : (u16)max(arr_a, arr_b);
        u16 new_arr = (u16)(base_arr + (u16)gate_delay);
        shared_arrival[threadIdx.x] = new_arr;
      }
      __syncthreads();
      // hier[1..3]: shared memory reduction + arrival tracking
      u32 tmp_cur_hi;
      u32 tmp_cur_hi_x = 0;
      u16 tmp_cur_arr = 0;
      for(int hi = 1; hi <= 3; ++hi) {
        int hier_width = 1 << (7 - hi);
        if(threadIdx.x >= hier_width && threadIdx.x < hier_width * 2) {
          u32 hier_input_a = shared_state[threadIdx.x + hier_width];
          u32 hier_input_b = shared_state[threadIdx.x + hier_width * 2];
          u32 a_eff = hier_input_a ^ hier_flag_xora;
          u32 b_eff = (hier_input_b ^ hier_flag_xorb) | hier_flag_orb;
          u32 ret = a_eff & b_eff;
          tmp_cur_hi = ret;
          shared_state[threadIdx.x] = ret;

          if(is_x_capable) {
            u32 a_x = shared_state_x[threadIdx.x + hier_width];
            u32 b_x = shared_state_x[threadIdx.x + hier_width * 2];
            u32 b_eff_x = b_x & ~hier_flag_orb;
            u32 ret_x = (a_x | b_eff_x) & (a_eff | a_x) & (b_eff | b_eff_x);
            tmp_cur_hi_x = ret_x;
            shared_state_x[threadIdx.x] = ret_x;
          }

          // Arrival tracking (delay added even for pass-throughs)
          u16 arr_a = (u16)shared_arrival[threadIdx.x + hier_width];
          u16 arr_b = (u16)shared_arrival[threadIdx.x + hier_width * 2];
          bool is_pass = (hier_flag_orb == 0xFFFFFFFF);
          u16 base_arr = is_pass ? arr_a : (u16)max(arr_a, arr_b);
          u16 new_arr = (u16)(base_arr + (u16)gate_delay);
          tmp_cur_arr = new_arr;
          shared_arrival[threadIdx.x] = tmp_cur_arr;
        }
        __syncthreads();
      }
      // hier[4..7], within the first warp.
      // Pack arrival into u32 for warp shuffle
      u32 tmp_cur_arr_u32 = (u32)tmp_cur_arr;
      if(threadIdx.x < 32) {
        for(int hi = 4; hi <= 7; ++hi) {
          int hier_width = 1 << (7 - hi);
          u32 hier_input_a = __shfl_down_sync(LANE_MASK_ALL, tmp_cur_hi, hier_width);
          u32 hier_input_b = __shfl_down_sync(LANE_MASK_ALL, tmp_cur_hi, hier_width * 2);
          u32 hier_a_x = is_x_capable ? __shfl_down_sync(LANE_MASK_ALL, tmp_cur_hi_x, hier_width) : 0;
          u32 hier_b_x = is_x_capable ? __shfl_down_sync(LANE_MASK_ALL, tmp_cur_hi_x, hier_width * 2) : 0;
          u32 arr_a_u32 = __shfl_down_sync(LANE_MASK_ALL, tmp_cur_arr_u32, hier_width);
          u32 arr_b_u32 = __shfl_down_sync(LANE_MASK_ALL, tmp_cur_arr_u32, hier_width * 2);
          if(threadIdx.x >= hier_width && threadIdx.x < hier_width * 2) {
            u32 a_eff = hier_input_a ^ hier_flag_xora;
            u32 b_eff = (hier_input_b ^ hier_flag_xorb) | hier_flag_orb;
            tmp_cur_hi = a_eff & b_eff;
            if(is_x_capable) {
              u32 b_eff_x = hier_b_x & ~hier_flag_orb;
              tmp_cur_hi_x = (hier_a_x | b_eff_x) & (a_eff | hier_a_x) & (b_eff | b_eff_x);
            }
            // Arrival tracking (delay added even for pass-throughs)
            bool is_pass = (hier_flag_orb == 0xFFFFFFFF);
            u16 base_arr = is_pass ? (u16)arr_a_u32 : (u16)max((u16)arr_a_u32, (u16)arr_b_u32);
            u16 new_arr = (u16)(base_arr + (u16)gate_delay);
            tmp_cur_arr_u32 = (u32)new_arr;
          }
        }
        u32 v1 = __shfl_down_sync(LANE_MASK_ALL, tmp_cur_hi, 1);
        u32 v1_x = is_x_capable ? __shfl_down_sync(LANE_MASK_ALL, tmp_cur_hi_x, 1) : 0;
        // hier[8..12]: bit-level operations within single u32
        // All 32 signals share one thread's arrival — arrival carries forward unchanged
        if(threadIdx.x == 0) {
          // Value lane
          u32 r8 = ((v1 << 16) ^ hier_flag_xora) & ((v1 ^ hier_flag_xorb) | hier_flag_orb) & 0xffff0000;
          u32 r9 = ((r8 >> 8) ^ hier_flag_xora) & (((r8 >> 16) ^ hier_flag_xorb) | hier_flag_orb) & 0xff00;
          u32 r10 = ((r9 >> 4) ^ hier_flag_xora) & (((r9 >> 8) ^ hier_flag_xorb) | hier_flag_orb) & 0xf0;
          u32 r11 = ((r10 >> 2) ^ hier_flag_xora) & (((r10 >> 4) ^ hier_flag_xorb) | hier_flag_orb) & 12;
          u32 r12 = ((r11 >> 1) ^ hier_flag_xora) & (((r11 >> 2) ^ hier_flag_xorb) | hier_flag_orb) & 2;
          tmp_cur_hi = r8 | r9 | r10 | r11 | r12;

          if(is_x_capable) {
            // X-mask lane: same structure with X-prop AND formula
            u32 r8_x, r9_x, r10_x, r11_x, r12_x;
            {
              u32 ae = (v1 << 16) ^ hier_flag_xora;
              u32 be = (v1 ^ hier_flag_xorb) | hier_flag_orb;
              u32 ax = v1_x << 16;
              u32 bex = v1_x & ~hier_flag_orb;
              r8_x = ((ax | bex) & (ae | ax) & (be | bex)) & 0xffff0000u;
            }
            {
              u32 ae = (r8 >> 8) ^ hier_flag_xora;
              u32 be = ((r8 >> 16) ^ hier_flag_xorb) | hier_flag_orb;
              u32 ax = r8_x >> 8;
              u32 bex = (r8_x >> 16) & ~hier_flag_orb;
              r9_x = ((ax | bex) & (ae | ax) & (be | bex)) & 0xff00u;
            }
            {
              u32 ae = (r9 >> 4) ^ hier_flag_xora;
              u32 be = ((r9 >> 8) ^ hier_flag_xorb) | hier_flag_orb;
              u32 ax = r9_x >> 4;
              u32 bex = (r9_x >> 8) & ~hier_flag_orb;
              r10_x = ((ax | bex) & (ae | ax) & (be | bex)) & 0xf0u;
            }
            {
              u32 ae = (r10 >> 2) ^ hier_flag_xora;
              u32 be = ((r10 >> 4) ^ hier_flag_xorb) | hier_flag_orb;
              u32 ax = r10_x >> 2;
              u32 bex = (r10_x >> 4) & ~hier_flag_orb;
              r11_x = ((ax | bex) & (ae | ax) & (be | bex)) & 0xcu;
            }
            {
              u32 ae = (r11 >> 1) ^ hier_flag_xora;
              u32 be = ((r11 >> 2) ^ hier_flag_xorb) | hier_flag_orb;
              u32 ax = r11_x >> 1;
              u32 bex = (r11_x >> 2) & ~hier_flag_orb;
              r12_x = ((ax | bex) & (ae | ax) & (be | bex)) & 0x2u;
            }
            tmp_cur_hi_x = r8_x | r9_x | r10_x | r11_x | r12_x;
          }
        }
        shared_state[threadIdx.x] = tmp_cur_hi;
        shared_state_x[threadIdx.x] = tmp_cur_hi_x;
        shared_arrival[threadIdx.x] = (u16)tmp_cur_arr_u32;
      }
      __syncthreads();

      // write out
      if((writeout_hook_i >> 8) == bs_i) {
        shared_writeouts[threadIdx.x] = shared_state[writeout_hook_i & 255];
        if(is_x_capable) {
          shared_writeouts_x[threadIdx.x] = shared_state_x[writeout_hook_i & 255];
        }
        shared_writeout_arrival[threadIdx.x] = shared_arrival[writeout_hook_i & 255];
      }
    }
    __syncthreads();

    // sram & duplicate permutation
    u32 sram_duplicate_t = 0;
    u32 sram_duplicate_t_x = 0;
#define GEMV1_SHUF_SRAM_DUPL_K(k_outer, k_inner, t_shuffle) { \
      u32 k = k_outer * 4 + k_inner;                          \
      u32 t_shuffle_1_idx = t_shuffle & ((1 << 16) - 1);      \
      u32 t_shuffle_2_idx = t_shuffle >> 16;                  \
                                                              \
      sram_duplicate_t |=                                     \
        (shared_writeouts[t_shuffle_1_idx >> 5] >>            \
         (t_shuffle_1_idx & 31) & 1) << (k * 2);              \
      sram_duplicate_t |=                                     \
        (shared_writeouts[t_shuffle_2_idx >> 5] >>            \
         (t_shuffle_2_idx & 31) & 1) << (k * 2 + 1);          \
      if(is_x_capable) {                                      \
        sram_duplicate_t_x |=                                 \
          (shared_writeouts_x[t_shuffle_1_idx >> 5] >>        \
           (t_shuffle_1_idx & 31) & 1) << (k * 2);            \
        sram_duplicate_t_x |=                                 \
          (shared_writeouts_x[t_shuffle_2_idx >> 5] >>        \
           (t_shuffle_2_idx & 31) & 1) << (k * 2 + 1);        \
      }                                                       \
    }
#define GEMV1_SHUF_SRAM_DUPL_K_4(k_outer, t_shuffle) {  \
      GEMV1_SHUF_SRAM_DUPL_K(k_outer, 0, t_shuffle.c1); \
      GEMV1_SHUF_SRAM_DUPL_K(k_outer, 1, t_shuffle.c2); \
      GEMV1_SHUF_SRAM_DUPL_K(k_outer, 2, t_shuffle.c3); \
      GEMV1_SHUF_SRAM_DUPL_K(k_outer, 3, t_shuffle.c4); \
    }
    script_pi += 256 * 4 * 5;
    GEMV1_SHUF_SRAM_DUPL_K_4(0, t4_1);
    t4_1.read(((const VectorRead4 *)(script + script_pi)) + threadIdx.x);
    GEMV1_SHUF_SRAM_DUPL_K_4(1, t4_2);
    t4_2.read(((const VectorRead4 *)(script + script_pi + 256 * 4)) + threadIdx.x);
    GEMV1_SHUF_SRAM_DUPL_K_4(2, t4_3);
    t4_3.read(((const VectorRead4 *)(script + script_pi + 256 * 4 * 2)) + threadIdx.x);
    GEMV1_SHUF_SRAM_DUPL_K_4(3, t4_4);
    t4_4.read(((const VectorRead4 *)(script + script_pi + 256 * 4 * 3)) + threadIdx.x);
#undef GEMV1_SHUF_SRAM_DUPL_K_4
#undef GEMV1_SHUF_SRAM_DUPL_K
    sram_duplicate_t = (sram_duplicate_t & ~t4_5.c2) ^ t4_5.c1;
    if(is_x_capable) {
      sram_duplicate_t_x = sram_duplicate_t_x & ~t4_5.c2; // set0 clears X too
    }
    t4_5.read(((const VectorRead4 *)(script + script_pi + 256 * 4 * 4)) + threadIdx.x);

    // sram read fires here.
    u32 *ram = nullptr;
    u32 *ram_x = nullptr;
    u32 r = 0, w0 = 0, r_x = 0, w0_x = 0;
    u32 port_w_addr_iv, port_w_wr_en, port_w_wr_data_iv;
    u32 port_w_wr_data_x = 0;
    if(threadIdx.x < num_srams * 4) {
      u32 addrs = sram_duplicate_t;
      u32 last_tid = 32 + threadIdx.x / 32 * 32;
      lane_mask_t mask = (last_tid <= num_srams * 4)
        ? LANE_MASK_ALL : (LANE_MASK_ALL >> (last_tid - num_srams * 4));
      port_w_wr_en = __shfl_down_sync(mask, sram_duplicate_t, 1);
      port_w_wr_data_iv = __shfl_down_sync(mask, sram_duplicate_t, 2);
      if(is_x_capable) {
        port_w_wr_data_x = __shfl_down_sync(mask, sram_duplicate_t_x, 2);
      }

      if(threadIdx.x % 4 == 0) {
        u32 sram_i = threadIdx.x / 4;
        u32 sram_st = sram_offset + sram_i * (1 << 13);
        u32 port_r_addr_iv = addrs & 0xffff;
        port_w_addr_iv = addrs >> 16;

        ram = sram_data + sram_st;
        r = ram[port_r_addr_iv];
        w0 = ram[port_w_addr_iv];
        if(is_x_capable && sram_xmask != nullptr) {
          ram_x = sram_xmask + sram_st;
          r_x = ram_x[port_r_addr_iv];
          w0_x = ram_x[port_w_addr_iv];
        }
      }
    }
    // __syncthreads();

    // clock enable permutation
    u32 clken_perm = 0;
    u32 clken_perm_x = 0;
#define GEMV1_SHUF_CLKEN_K(k_outer, k_inner, t_shuffle) { \
      u32 k = k_outer * 4 + k_inner;                      \
      u32 t_shuffle_1_idx = t_shuffle & ((1 << 16) - 1);  \
      u32 t_shuffle_2_idx = t_shuffle >> 16;              \
                                                          \
      clken_perm |=                                       \
        (shared_writeouts[t_shuffle_1_idx >> 5] >>        \
         (t_shuffle_1_idx & 31) & 1) << (k * 2);          \
      clken_perm |=                                       \
        (shared_writeouts[t_shuffle_2_idx >> 5] >>        \
         (t_shuffle_2_idx & 31) & 1) << (k * 2 + 1);      \
      if(is_x_capable) {                                  \
        clken_perm_x |=                                   \
          (shared_writeouts_x[t_shuffle_1_idx >> 5] >>    \
           (t_shuffle_1_idx & 31) & 1) << (k * 2);        \
        clken_perm_x |=                                   \
          (shared_writeouts_x[t_shuffle_2_idx >> 5] >>    \
           (t_shuffle_2_idx & 31) & 1) << (k * 2 + 1);    \
      }                                                   \
    }
#define GEMV1_SHUF_CLKEN_K_4(k_outer, t_shuffle) {  \
      GEMV1_SHUF_CLKEN_K(k_outer, 0, t_shuffle.c1); \
      GEMV1_SHUF_CLKEN_K(k_outer, 1, t_shuffle.c2); \
      GEMV1_SHUF_CLKEN_K(k_outer, 2, t_shuffle.c3); \
      GEMV1_SHUF_CLKEN_K(k_outer, 3, t_shuffle.c4); \
    }
    script_pi += 256 * 4 * 5;
    GEMV1_SHUF_CLKEN_K_4(0, t4_1);
    GEMV1_SHUF_CLKEN_K_4(1, t4_2);
    GEMV1_SHUF_CLKEN_K_4(2, t4_3);
    GEMV1_SHUF_CLKEN_K_4(3, t4_4);
#undef GEMV1_SHUF_CLKEN_K
#undef GEMV1_SHUF_CLKEN_K_4

    // sram commit
    if(threadIdx.x < num_srams * 4) {
      if(threadIdx.x % 4 == 0) {
        u32 sram_i = threadIdx.x / 4;
        shared_writeouts[num_ios - num_srams + sram_i] = r;
        ram[port_w_addr_iv] = (w0 & ~port_w_wr_en) | (port_w_wr_data_iv & port_w_wr_en);
        if(is_x_capable) {
          shared_writeouts_x[num_ios - num_srams + sram_i] = r_x;
          if(ram_x != nullptr) {
            ram_x[port_w_addr_iv] = (w0_x & ~port_w_wr_en) | (port_w_wr_data_x & port_w_wr_en);
          }
        }
      }
    }
    else if(threadIdx.x < num_srams * 4 + num_output_duplicates) {
      shared_writeouts[num_ios - num_srams - num_output_duplicates + (threadIdx.x - num_srams * 4)] = sram_duplicate_t;
      if(is_x_capable) {
        shared_writeouts_x[num_ios - num_srams - num_output_duplicates + (threadIdx.x - num_srams * 4)] = sram_duplicate_t_x;
      }
    }

    __syncthreads();
    u32 writeout_inv = shared_writeouts[threadIdx.x];
    u32 writeout_inv_x = is_x_capable ? shared_writeouts_x[threadIdx.x] : 0;

    clken_perm = (clken_perm & ~t4_5.c2) ^ t4_5.c1;
    if(is_x_capable) {
      clken_perm_x = clken_perm_x & ~t4_5.c2; // set0 clears X
    }
    writeout_inv ^= t4_5.c3;
    // data_inv (t4_5.c3) doesn't affect X-mask (inversion preserves X)

    if(threadIdx.x < num_ios) {
      u32 old_wo = input_state[io_offset + threadIdx.x];
      u32 wo = (old_wo & ~clken_perm) | (writeout_inv & clken_perm);
      output_state[io_offset + threadIdx.x] = wo;

      if(is_x_capable) {
        u32 old_x = input_state[xmask_state_offset + io_offset + threadIdx.x];
        // X-mask DFF gating: X clken → output is X
        u32 wo_x = (old_x & ~clken_perm & ~clken_perm_x)
                 | (writeout_inv_x & clken_perm & ~clken_perm_x)
                 | clken_perm_x;
        output_state[xmask_state_offset + io_offset + threadIdx.x] = wo_x;
      }

      // Write arrival time to global memory for timed VCD output
      if(arrival_state_offset != 0) {
        output_state[arrival_state_offset + io_offset + threadIdx.x] =
            (u32)shared_writeout_arrival[threadIdx.x];
      }
    }

    // DFF timing violation check (per writeout word)
    if(threadIdx.x < num_ios && clken_perm != 0 && timing_constraints != nullptr) {
      u32 constraint = timing_constraints[io_offset + threadIdx.x];
      if(constraint != 0) {
        u16 setup_ps = (u16)(constraint >> 16);
        u16 hold_ps = (u16)(constraint & 0xFFFF);
        u16 arrival = shared_writeout_arrival[threadIdx.x];
        // Setup: arrival + setup must fit within clock period
        if(arrival > 0 && (u32)arrival + (u32)setup_ps > clock_period_ps) {
          int slack = (int)clock_period_ps - (int)arrival - (int)setup_ps;
          write_event(event_buffer, EVENT_TYPE_SETUP_VIOLATION,
                     io_offset + threadIdx.x, cycle_i,
                     io_offset + threadIdx.x, (u32)slack, (u32)arrival, (u32)setup_ps);
        }
        // Hold: arrival must exceed hold time
        if(arrival < hold_ps) {
          int slack = (int)arrival - (int)hold_ps;
          write_event(event_buffer, EVENT_TYPE_HOLD_VIOLATION,
                     io_offset + threadIdx.x, cycle_i,
                     io_offset + threadIdx.x, (u32)slack, (u32)arrival, (u32)hold_ps);
        }
      }
    }

    if(is_last_part) break;
  }
  assert(script_size == script_pi);
}

__global__ void simulate_v1_noninteractive_simple_scan(
  usize num_blocks,
  usize num_major_stages,
  const usize *__restrict__ blocks_start,
  const u32 *__restrict__ blocks_data,
  u32 *__restrict__ sram_data,
  u32 *__restrict__ sram_xmask,
  usize num_cycles,
  usize state_size,
  u32 *__restrict__ states_noninteractive,
  const u32 *__restrict__ timing_constraints,
  EventBuffer *__restrict__ event_buffer,
  i32 arrival_state_offset
  )
{
  assert(num_blocks == gridDim.x);
  assert(256 == blockDim.x);
  __shared__ u32 shared_metadata[256];
  __shared__ u32 shared_writeouts[256];
  __shared__ u32 shared_state[256];
  __shared__ u32 shared_writeouts_x[256];  // X-mask sideband for writeouts
  __shared__ u32 shared_state_x[256];      // X-mask sideband for state
  __shared__ u16 shared_arrival[256];  // Per-thread-position arrival times (raw picoseconds)
  __shared__ u16 shared_writeout_arrival[256];  // Arrival times captured at writeout
  shared_writeout_arrival[threadIdx.x] = 0;  // Must initialize — only writeout threads update this
  shared_writeouts_x[threadIdx.x] = 0;
  shared_state_x[threadIdx.x] = 0;
  __shared__ u32 script_starts[32], script_sizes[32];
  assert(num_major_stages <= 32);

  // Read clock period from first element of constraints buffer
  u32 clock_period_ps = (timing_constraints != nullptr) ? timing_constraints[0] : 0;
  const u32* constraints_data = (timing_constraints != nullptr) ? timing_constraints + 1 : nullptr;

  if(threadIdx.x < num_major_stages) {
    script_starts[threadIdx.x] = blocks_start[threadIdx.x * num_blocks + blockIdx.x];
    script_sizes[threadIdx.x] = blocks_start[threadIdx.x * num_blocks + blockIdx.x + 1] - script_starts[threadIdx.x];
  }
  __syncthreads();
  for(usize cycle_i = 0; cycle_i < num_cycles; ++cycle_i) {
    for(usize stage_i = 0; stage_i < num_major_stages; ++stage_i) {
      simulate_block_v1(
        blocks_data + script_starts[stage_i],
        script_sizes[stage_i],
        states_noninteractive + cycle_i * state_size,
        states_noninteractive + (cycle_i + 1) * state_size,
        sram_data,
        sram_xmask,
        shared_metadata, shared_writeouts, shared_state,
        shared_writeouts_x, shared_state_x,
        shared_arrival,
        shared_writeout_arrival,
        constraints_data,
        clock_period_ps,
        event_buffer,
        (u32)cycle_i,
        arrival_state_offset
        );
      cooperative_groups::this_grid().sync();
    }
  }
}

// ─────────────────────────────────────────────────────────────────────────────
// cosim kernels (CUDA/HIP port of the Metal cosim path; #105 Phase 2 / 2a)
//
// Unlike the `sim` scan above, cosim is reactive (inputs depend on outputs), so
// the host drives one scheduler edge at a time over a 2-slot [input|output]
// state. These kernels are NON-cooperative ordinary launches — the host loops
// major stages and each launch is the grid-wide barrier — so cosim never needs
// the cooperative grid.sync the scan relies on. They mirror Metal's `state_prep`
// and `simulate_v1_stage` (csrc/kernel_v1.metal).
// ─────────────────────────────────────────────────────────────────────────────

// state_prep: copy output slot → input slot, then apply this edge's bit ops.
// `ops` is a flat array of `num_ops` (position, value) u32 pairs (the Rust
// `BitOp` is #[repr(C)] { position: u32, value: u32 }, so a &[BitOp] reinterprets
// directly as this flat u32 view). Single block of 256 threads.
__global__ void cosim_state_prep(
  u32 *__restrict__ states,
  u32 state_size,
  u32 num_ops,
  u32 xmask_state_offset,  // X-mask word offset within a slot (0 = xprop off)
  const u32 *__restrict__ ops
  )
{
  // Step 1: copy output slot (states[state_size..2*state_size]) → input slot.
  for (u32 i = threadIdx.x; i < state_size; i += 256) {
    states[i] = states[state_size + i];
  }

  // Ensure the copy is globally visible before thread 0 modifies bits.
  __threadfence();
  __syncthreads();

  // Step 2: apply bit ops to the input slot (thread 0 only).
  if (threadIdx.x == 0) {
    for (u32 i = 0; i < num_ops; i++) {
      u32 pos = ops[2 * i];
      u32 val = ops[2 * i + 1];
      u32 word_idx = pos >> 5;
      u32 bit_mask = 1u << (pos & 31u);
      if (val != 0) {
        states[word_idx] |= bit_mask;
      } else {
        states[word_idx] &= ~bit_mask;
      }
      // Driving a bit makes it known: clear its X-mask (#95 phase 3).
      if (xmask_state_offset != 0u) {
        states[xmask_state_offset + word_idx] &= ~bit_mask;
      }
    }
  }
}

// simulate_v1_stage: evaluate ONE major stage of ONE cycle. Reads the state
// slot for `current_cycle`, writes the slot for `current_cycle + 1`. The host
// loops stages (and, for `sim`, cycles), each launch acting as the barrier that
// `simulate_v1_noninteractive_simple_scan` gets from its cooperative grid.sync.
// Grid = num_blocks × 256.
//
// Serves both callers, hence not `cosim_*`:
//
//   * cosim passes current_cycle = 0 over a 2-slot [input | output] buffer: it
//     is reactive and advances one scheduler edge at a time, so the slot it
//     writes is always slot 1.
//   * `sim`'s non-cooperative fallback passes the real cycle index over the
//     full (num_cycles + 1) × state_size buffer, which is the same addressing
//     the scan kernel does internally.
//
// Mirrors Metal's `simulate_v1_stage` (csrc/kernel_v1.metal), parameterised the
// same way — Metal has no device-wide barrier either.
__global__ void simulate_v1_stage(
  usize num_blocks,
  const usize *__restrict__ blocks_start,
  const u32 *__restrict__ blocks_data,
  u32 *__restrict__ sram_data,
  u32 *__restrict__ sram_xmask,
  usize state_size,
  u32 *__restrict__ states,        // cosim: 2 slots [input | output]
                                   // sim:   (num_cycles + 1) slots
  usize current_cycle,
  usize current_stage,
  const u32 *__restrict__ timing_constraints,  // nullptr if timing off
  EventBuffer *__restrict__ event_buffer,      // nullptr if timing off
  i32 arrival_state_offset
  )
{
  assert(num_blocks == gridDim.x);
  assert(256 == blockDim.x);
  __shared__ u32 shared_metadata[256];
  __shared__ u32 shared_writeouts[256];
  __shared__ u32 shared_state[256];
  __shared__ u32 shared_writeouts_x[256];  // X-mask sideband for writeouts
  __shared__ u32 shared_state_x[256];      // X-mask sideband for state
  __shared__ u16 shared_arrival[256];
  __shared__ u16 shared_writeout_arrival[256];
  shared_writeout_arrival[threadIdx.x] = 0;
  shared_writeouts_x[threadIdx.x] = 0;
  shared_state_x[threadIdx.x] = 0;

  u32 clock_period_ps = (timing_constraints != nullptr) ? timing_constraints[0] : 0;
  const u32 *constraints_data =
    (timing_constraints != nullptr) ? timing_constraints + 1 : nullptr;

  u32 script_start = blocks_start[current_stage * num_blocks + blockIdx.x];
  u32 script_size =
    blocks_start[current_stage * num_blocks + blockIdx.x + 1] - script_start;
  __syncthreads();

  simulate_block_v1(
    blocks_data + script_start,
    script_size,
    states + current_cycle * state_size,        // input slot
    states + (current_cycle + 1) * state_size,  // output slot
    sram_data,
    sram_xmask,
    shared_metadata, shared_writeouts, shared_state,
    shared_writeouts_x, shared_state_x,
    shared_arrival,
    shared_writeout_arrival,
    constraints_data,
    clock_period_ps,
    event_buffer,
    (u32)current_cycle,     // 0 for cosim (one transition at a time), the real
                            // cycle index for `sim`'s fallback
    arrival_state_offset
    );
}

// ─── Cosim GPU peripherals: gpu_io_step (UART + bus trace) ───────────────────
//
// Ported from csrc/kernel_v1.metal (Metal `gpu_io_step`, struct catalogue
// :1041-1163, kernel :1170). The structs below mirror the shared Rust
// `#[repr(C)]` ABI in src/sim/cosim/mod.rs ("Shared GPU peripheral ABI") field-
// for-field; the static_asserts pin the byte counts to the same literals the
// Rust `size_of` asserts use. Any field-order/padding drift between these three
// definitions silently corrupts the cross-FFI buffers — keep them in lockstep.
//
// The CPU equivalence oracle is `CpuBackend`'s UART/bus FSM (mod.rs); these
// kernels must produce byte-identical drains to it (and to Metal).

#define COSIM_MAX_UARTS 4
#define COSIM_UART_CHANNEL_CAP 4096

#define COSIM_WB_TRACE_MAX_ADR_BITS 30
#define COSIM_WB_TRACE_MAX_DAT_BITS 32
#define COSIM_WB_TRACE_CHANNEL_CAP 16384

#define COSIM_MAX_BUS_TRACES 4
#define COSIM_BUS_TRACE_MAX_ADR_BITS 32
#define COSIM_BUS_TRACE_MAX_DAT_BITS 32
#define COSIM_BUS_TRACE_CHANNEL_CAP 16384

#define COSIM_BUS_PROTO_APB3 0u
#define COSIM_BUS_PROTO_AHB_LITE 1u
#define COSIM_BUS_PROTO_AHB5 2u

struct UartDecoderState {
  u32 state;          // 0=IDLE, 1=START, 2=DATA, 3=STOP
  u32 last_tx;
  u32 start_cycle;
  u32 bits_received;
  u32 value;
  u32 current_cycle;  // incremented each call
};
static_assert(sizeof(UartDecoderState) == 24, "UartDecoderState ABI");

struct UartPerChannelConfig {
  u32 tx_out_pos;
  u32 cycles_per_bit;
};
static_assert(sizeof(UartPerChannelConfig) == 8, "UartPerChannelConfig ABI");

struct UartParams {
  u32 state_size;
  u32 n_uarts;          // 0 = skip, up to COSIM_MAX_UARTS
  u32 _pad[2];
  UartPerChannelConfig channels[COSIM_MAX_UARTS];
};
static_assert(sizeof(UartParams) == 48, "UartParams ABI");

struct UartChannel {
  u32 write_head;       // CPU reads this to know how many bytes are available
  u32 capacity;
  u32 _pad[2];
  unsigned char data[COSIM_UART_CHANNEL_CAP];
};
static_assert(sizeof(UartChannel) == 4112, "UartChannel ABI");

struct WbTraceParams {
  u32 ibus_cyc_pos;
  u32 ibus_stb_pos;
  u32 ibus_adr_pos[COSIM_WB_TRACE_MAX_ADR_BITS];
  u32 ibus_rdata_pos[COSIM_WB_TRACE_MAX_DAT_BITS];
  u32 dbus_cyc_pos;
  u32 dbus_stb_pos;
  u32 dbus_we_pos;
  u32 dbus_adr_pos[COSIM_WB_TRACE_MAX_ADR_BITS];
  u32 spiflash_ack_pos;
  u32 sram_ack_pos;
  u32 csr_ack_pos;
  u32 has_trace;        // 0 = skip
};
static_assert(sizeof(WbTraceParams) == 404, "WbTraceParams ABI");

struct WbTraceEntry {
  u32 tick;
  u32 flags;
  u32 ibus_adr;
  u32 ibus_rdata;
  u32 dbus_adr;
};
static_assert(sizeof(WbTraceEntry) == 20, "WbTraceEntry ABI");

struct WbTraceChannel {
  u32 write_head;
  u32 capacity;
  u32 current_tick;
  u32 prev_flags;
  // entries[capacity] follow immediately in memory (at byte offset 16)
};
static_assert(sizeof(WbTraceChannel) == 16, "WbTraceChannel ABI");

struct BusTraceParams {
  u32 protocol;
  u32 addr_bits;
  u32 data_bits;
  u32 sel_pos;     // APB psel
  u32 enable_pos;  // APB penable
  u32 ready_pos;   // APB pready (0xFFFFFFFF → treated as 1)
  u32 write_pos;   // APB pwrite
  u32 resp_pos;    // APB pslverr
  u32 addr_pos[COSIM_BUS_TRACE_MAX_ADR_BITS];
  u32 wdata_pos[COSIM_BUS_TRACE_MAX_DAT_BITS];
  u32 rdata_pos[COSIM_BUS_TRACE_MAX_DAT_BITS];
};
static_assert(sizeof(BusTraceParams) == 416, "BusTraceParams ABI");

struct BusTraceParamsAll {
  u32 n_buses;
  u32 _pad[3];
  BusTraceParams buses[COSIM_MAX_BUS_TRACES];
};
static_assert(sizeof(BusTraceParamsAll) == 1680, "BusTraceParamsAll ABI");

struct BusTraceEntry {
  u32 tick;
  u32 flags;   // [0]=write [1]=err [8..15]=bus_id
  u32 addr;
  u32 wdata;
  u32 rdata;
};
static_assert(sizeof(BusTraceEntry) == 20, "BusTraceEntry ABI");

struct BusTraceChannel {
  u32 write_head;
  u32 capacity;
  u32 current_tick;
  u32 prev_gate;   // bit i = bus i's gate level last tick (rising-edge detect)
  // entries[capacity] follow immediately in memory (at byte offset 16)
};
static_assert(sizeof(BusTraceChannel) == 16, "BusTraceChannel ABI");

// gpu_io_step: combined UART-TX decoder + Wishbone + AHB/APB bus trace. Runs
// once per scheduler edge (thread 0 only), reading the OUTPUT state slot. Direct
// transliteration of kernel_v1.metal:1170; cross-checked against the CpuBackend
// FSM (mod.rs:3923). `current_cycle`/`current_tick` advance one per call —
// the host loops edges, so the kernel never sees the batch.
__global__ void gpu_io_step(
  u32 *__restrict__ states,
  UartDecoderState *__restrict__ uart_state,
  const UartParams *__restrict__ uart_params,
  UartChannel *__restrict__ uart_channel,
  WbTraceChannel *__restrict__ wb_channel,
  const WbTraceParams *__restrict__ wb_params,
  BusTraceChannel *__restrict__ bus_channel,
  const BusTraceParamsAll *__restrict__ bus_params
  )
{
  if (threadIdx.x != 0) return;

  u32 state_size = uart_params->state_size;

  // Read a bit from the OUTPUT state slot (0xFFFFFFFF position → 0).
  #define READ_OUT_BIT(pos) \
      (((pos) != 0xFFFFFFFFu) ? ((states[state_size + ((pos) >> 5)] >> ((pos) & 31u)) & 1u) : 0u)

  // ── UART TX decoder (multi-instance) ──────────────────────────────────
  for (u32 ui = 0; ui < uart_params->n_uarts && ui < COSIM_MAX_UARTS; ui++) {
    u32 cycles_per_bit = uart_params->channels[ui].cycles_per_bit;
    u32 tx = READ_OUT_BIT(uart_params->channels[ui].tx_out_pos);

    UartDecoderState *us = &uart_state[ui];
    UartChannel *uc = &uart_channel[ui];

    u32 cycle = us->current_cycle;
    u32 st = us->state;
    u32 last_tx = us->last_tx;
    u32 start_cycle = us->start_cycle;
    u32 bits_received = us->bits_received;
    u32 value = us->value;

    if (st == 0) {
      if (last_tx == 1 && tx == 0) {
        st = 1;
        start_cycle = cycle;
      }
    } else if (st == 1) {
      if (cycle >= start_cycle + cycles_per_bit / 2) {
        if (tx == 0) {
          st = 2;
          start_cycle = start_cycle + cycles_per_bit;
          bits_received = 0;
          value = 0;
        } else {
          st = 0;
        }
      }
    } else if (st == 2) {
      u32 bit_center = start_cycle + bits_received * cycles_per_bit + cycles_per_bit / 2;
      if (cycle >= bit_center) {
        value = value | (tx << bits_received);
        if (bits_received >= 7) {
          st = 3;
          start_cycle = start_cycle + 8 * cycles_per_bit;
        } else {
          bits_received = bits_received + 1;
        }
      }
    } else if (st == 3) {
      if (cycle >= start_cycle + cycles_per_bit / 2) {
        if (tx == 1) {
          u32 head = uc->write_head;
          u32 cap = uc->capacity;
          uc->data[head % cap] = (unsigned char)(value & 0xFF);
          uc->write_head = head + 1;
        }
        st = 0;
      }
    }

    us->state = st;
    us->last_tx = tx;
    us->start_cycle = start_cycle;
    us->bits_received = bits_received;
    us->value = value;
    us->current_cycle = cycle + 1;
  }

  // ── Wishbone bus trace ─────────────────────────────────────────────────
  if (wb_params->has_trace != 0) {
    u32 ibus_cyc = READ_OUT_BIT(wb_params->ibus_cyc_pos);
    u32 ibus_stb = READ_OUT_BIT(wb_params->ibus_stb_pos);
    u32 dbus_cyc = READ_OUT_BIT(wb_params->dbus_cyc_pos);
    u32 dbus_stb = READ_OUT_BIT(wb_params->dbus_stb_pos);
    u32 dbus_we  = READ_OUT_BIT(wb_params->dbus_we_pos);
    u32 spi_ack  = READ_OUT_BIT(wb_params->spiflash_ack_pos);
    u32 sram_ack = READ_OUT_BIT(wb_params->sram_ack_pos);
    u32 csr_ack  = READ_OUT_BIT(wb_params->csr_ack_pos);

    u32 flags = (ibus_cyc) | (ibus_stb << 1) | (dbus_cyc << 2) | (dbus_stb << 3)
              | (dbus_we << 4) | (spi_ack << 5) | (sram_ack << 6) | (csr_ack << 7);

    u32 tick = wb_channel->current_tick;
    bool active = (ibus_cyc && ibus_stb) || (dbus_cyc && dbus_stb);
    bool changed = (flags != wb_channel->prev_flags);

    if (active || changed) {
      u32 ibus_adr = 0;
      for (u32 i = 0; i < COSIM_WB_TRACE_MAX_ADR_BITS; i++) {
        ibus_adr |= READ_OUT_BIT(wb_params->ibus_adr_pos[i]) << i;
      }
      u32 ibus_rdata = 0;
      for (u32 i = 0; i < COSIM_WB_TRACE_MAX_DAT_BITS; i++) {
        ibus_rdata |= READ_OUT_BIT(wb_params->ibus_rdata_pos[i]) << i;
      }
      u32 dbus_adr = 0;
      for (u32 i = 0; i < COSIM_WB_TRACE_MAX_ADR_BITS; i++) {
        dbus_adr |= READ_OUT_BIT(wb_params->dbus_adr_pos[i]) << i;
      }

      u32 head = wb_channel->write_head;
      u32 cap = wb_channel->capacity;
      if (head < cap) {
        WbTraceEntry *entries = (WbTraceEntry *)((unsigned char *)wb_channel + 16);
        WbTraceEntry *e = &entries[head % cap];
        e->tick = tick;
        e->flags = flags;
        e->ibus_adr = ibus_adr;
        e->ibus_rdata = ibus_rdata;
        e->dbus_adr = dbus_adr;
        wb_channel->write_head = head + 1;
      }
    }

    wb_channel->prev_flags = flags;
    wb_channel->current_tick = tick + 1;
  }

  // ── Bus transaction trace (AHB/APB, ADR 0013) ──────────────────────────
  // Rising-edge detection records exactly one entry per completed transfer.
  if (bus_params->n_buses != 0u) {
    u32 btick = bus_channel->current_tick;
    u32 prev = bus_channel->prev_gate;
    u32 new_gate = 0u;
    for (u32 b = 0u; b < bus_params->n_buses && b < COSIM_MAX_BUS_TRACES; b++) {
      const BusTraceParams *bp = &bus_params->buses[b];
      u32 gate = 0u;
      if (bp->protocol == COSIM_BUS_PROTO_APB3) {
        u32 sel = READ_OUT_BIT(bp->sel_pos);
        u32 en  = READ_OUT_BIT(bp->enable_pos);
        u32 rdy = (bp->ready_pos == 0xFFFFFFFFu) ? 1u : READ_OUT_BIT(bp->ready_pos);
        gate = (sel & en & rdy);
      }
      // AHB-Lite / AHB5 gating: Phase 2.
      new_gate |= (gate << b);

      bool rising = (gate != 0u) && (((prev >> b) & 1u) == 0u);
      if (rising) {
        u32 write = READ_OUT_BIT(bp->write_pos);
        u32 err   = READ_OUT_BIT(bp->resp_pos);
        u32 addr = 0u;
        for (u32 i = 0u; i < COSIM_BUS_TRACE_MAX_ADR_BITS && i < bp->addr_bits; i++) {
          addr |= READ_OUT_BIT(bp->addr_pos[i]) << i;
        }
        u32 wdata = 0u;
        u32 rdata = 0u;
        for (u32 i = 0u; i < COSIM_BUS_TRACE_MAX_DAT_BITS && i < bp->data_bits; i++) {
          wdata |= READ_OUT_BIT(bp->wdata_pos[i]) << i;
          rdata |= READ_OUT_BIT(bp->rdata_pos[i]) << i;
        }
        u32 head = bus_channel->write_head;
        u32 cap = bus_channel->capacity;
        if (head < cap) {
          BusTraceEntry *entries = (BusTraceEntry *)((unsigned char *)bus_channel + 16);
          BusTraceEntry *e = &entries[head % cap];
          e->tick = btick;
          e->flags = (write & 1u) | ((err & 1u) << 1) | (b << 8);
          e->addr = addr;
          e->wdata = wdata;
          e->rdata = rdata;
          bus_channel->write_head = head + 1u;
        }
      }
    }
    bus_channel->prev_gate = new_gate;
    bus_channel->current_tick = btick + 1u;
  }

  #undef READ_OUT_BIT
}

// ─── Cosim GPU peripheral: SPI flash (gpu_apply_flash_din + gpu_flash_model_step) ─
//
// Ported from csrc/kernel_v1.metal (FlashState :738, FlashDinParams :758,
// FlashModelParams :764, flash_process_byte :775, flash_eval_commit_persistent
// :843, gpu_apply_flash_din :904, gpu_flash_model_step :943). Direct GPU
// reimplementation of the CPU `CppSpiFlash` FSM (spiflash_model.cc). The structs
// below mirror the shared Rust `#[repr(C)]` ABI in src/sim/cosim/mod.rs (FlashState
// :318 / FlashDinParams :341 / FlashModelParams :354) field-for-field; the
// static_asserts pin the byte counts to the same 48/24/32 literals the Rust
// `size_of` asserts use. Any field-order/padding drift between these three
// definitions silently corrupts the cross-FFI buffers — keep them in lockstep.
//
// Metal→CUDA primitive translations: Metal `uchar`→`u8`, `uint`→`u32`,
// `device`/`thread`/`constant` address spaces collapse to plain pointers/refs.
// These are single-thread (thread 0) kernels like gpu_io_step, so no __threadfence
// is needed (no cross-thread visibility within a kernel).

struct FlashState {
  i32 bit_count;       // bits received in current byte
  i32 byte_count;      // bytes in current transaction
  u32 data_width;      // 1 (SPI) or 4 (QSPI)
  u32 addr;            // 24-bit read address
  u8  curr_byte;       // byte accumulator
  u8  command;         // current command (0x03, 0xEB, etc.)
  u8  out_buffer;      // MISO shift register
  u8  _pad1;
  u32 prev_clk;        // model internal: last clk seen by flash_eval_commit
  u32 prev_csn;        // delayed csn: output csn from previous tick
  u8  d_i;             // current MISO output nibble (4 bits)
  u8  _pad2[3];
  u8  prev_d_out;      // previous MOSI for setup delay
  u8  _pad3[3];
  u32 in_reset;        // 1 = reset active, forces d_i=0x0F
  u32 last_error_cmd;  // nonzero if unknown command encountered
  u32 model_prev_csn;  // model internal: last csn seen by flash_eval_commit
  u32 qpi;             // QSPI PSRAM mode latch: 1 → command bytes sample 4-lane
};
static_assert(sizeof(FlashState) == 52, "FlashState ABI");

// Max QSPI memory instances (mirror MAX_QSPI_MEMS in src/sim/cosim/mod.rs).
#define MAX_QSPI_MEMS 4

struct FlashDinParams {
  u32 d_in_pos[4];        // input state bit positions for d0..d3
  u32 has_flash;          // 0 = skip
  u32 xmask_state_offset; // X-mask word offset (0 = xprop off)
};
static_assert(sizeof(FlashDinParams) == 24, "FlashDinParams ABI");

// Plural block (mirror BusTraceParamsAll): n_flashes + per-instance params.
struct FlashDinParamsAll {
  u32 n_flashes;
  u32 _pad[3];
  FlashDinParams flashes[MAX_QSPI_MEMS];
};
static_assert(sizeof(FlashDinParamsAll) == 16 + MAX_QSPI_MEMS * 24,
              "FlashDinParamsAll ABI");

struct FlashModelParams {
  u32 state_size;      // words per state slot
  u32 clk_out_pos;     // output state bit position: flash_clk
  u32 csn_out_pos;     // output state bit position: flash_csn
  u32 d_out_pos[4];    // output state bit positions: d0..d3
  u32 flash_data_size; // this instance's backing-store size in bytes
  u32 data_offset;     // byte offset of this instance's store in flash_data
  // ── QSPI PSRAM (RAM-mode) config — all zero/sentinel for plain flash ──
  u32 writable;        // 1 = writable store + qpi/quad-write enabled
  u32 enter_qpi_cmd;   // command latching QPI mode (0xFFFFFFFF = none)
  u32 quad_write_cmd;  // command quad-writing the store (0xFFFFFFFF = none)
  u32 qpi_read_dummy;  // dummy SCK cycles for a QPI 0xEB read (e.g. 6)
};
static_assert(sizeof(FlashModelParams) == 52, "FlashModelParams ABI");

struct FlashModelParamsAll {
  u32 n_flashes;
  u32 _pad[3];
  FlashModelParams flashes[MAX_QSPI_MEMS];
};
static_assert(sizeof(FlashModelParamsAll) == 16 + MAX_QSPI_MEMS * 52,
              "FlashModelParamsAll ABI");

// Process a completed byte (command decode + address accumulation + data lookup).
// Port of kernel_v1.metal:775. All "thread&" refs become plain device-local refs.
__device__ __forceinline__ void flash_process_byte(
  i32 &bit_count,
  i32 &byte_count,
  u32 &data_width,
  u32 &addr,
  u8 &curr_byte,
  u8 &command,
  u8 &out_buffer,
  u32 &last_error_cmd,
  u32 &qpi,             // QSPI PSRAM mode latch (read/write)
  u32 ram_writable,     // 1 = writable store + qpi/quad-write enabled
  u32 enter_qpi_cmd,    // command latching QPI mode (0xFFFFFFFF = none)
  u32 quad_write_cmd,   // command quad-writing the store (0xFFFFFFFF = none)
  u32 qpi_read_dummy,   // dummy SCK cycles for a QPI 0xEB read
  u8 *flash_data,       // writable in RAM mode; read-only for flash
  u32 flash_data_size
  )
{
  out_buffer = 0;
  if (byte_count == 0) {
    addr = 0;
    // In QPI mode even the command byte is 4-lane; else 1-lane (flash).
    data_width = (qpi != 0u) ? 4u : 1u;
    command = curr_byte;
    if (ram_writable != 0u && enter_qpi_cmd != 0xFFFFFFFFu
        && (u32)command == enter_qpi_cmd) {
      // Enter-QPI (single-lane): all later commands sample 4-lane.
      qpi = 1u;
    } else if (ram_writable != 0u && quad_write_cmd != 0xFFFFFFFFu
        && (u32)command == quad_write_cmd) {
      // Quad write: address + data phases are 4-lane.
      data_width = 4u;
    } else if (command == 0xab) {
      // power up - nothing to do
    } else if (command == 0x03 || command == 0x9f || command == 0xff
        || command == 0x35 || command == 0x31 || command == 0x50
        || command == 0x05 || command == 0x01 || command == 0x06) {
      // nothing to do
    } else if (command == 0xeb) {
      data_width = 4;
    } else {
      last_error_cmd = command;
    }
  } else {
    if (command == 0x03) {
      // Single read
      if (byte_count <= 3) {
        addr |= ((u32)curr_byte << ((3 - byte_count) * 8));
      }
      if (byte_count >= 3) {
        u32 idx = addr & 0x00FFFFFFu;
        if (idx < flash_data_size) {
          out_buffer = flash_data[idx];
        } else {
          out_buffer = 0xFF;
        }
        addr = (addr + 1) & 0x00FFFFFFu;
      }
    } else if (command == 0xeb) {
      // Quad read
      if (byte_count <= 3) {
        addr |= ((u32)curr_byte << ((3 - byte_count) * 8));
      }
      // In QPI mode the transaction is uniform 4-lane: cmd@posedges 0-1,
      // 24-bit addr@2-7, then qpi_read_dummy dummy posedges, then data.
      // out_buffer must be loaded on the posedge *before* the first data
      // posedge (driven on the following negedge, sampled next posedge), so
      // the threshold is 3 + qpi_read_dummy/2 (= 6 for the APS6404L
      // QRD_DUMMY=6). Plain flash keeps its byte_count>=6.
      u32 data_start = (qpi != 0u) ? (3u + (qpi_read_dummy >> 1)) : 6u;
      if ((u32)byte_count >= data_start) {
        u32 idx = addr & 0x00FFFFFFu;
        if (idx < flash_data_size) {
          out_buffer = flash_data[idx];
        } else {
          out_buffer = 0xFF;
        }
        addr = (addr + 1) & 0x00FFFFFFu;
      }
    } else if (ram_writable != 0u && quad_write_cmd != 0xFFFFFFFFu
        && (u32)command == quad_write_cmd) {
      // Quad write: 24-bit address in bytes 1..3 (MSB first), then data bytes
      // stored into the backing memory (4-lane).
      if (byte_count <= 3) {
        addr |= ((u32)curr_byte << ((3 - byte_count) * 8));
      }
      if (byte_count >= 4) {
        u32 idx = addr & 0x00FFFFFFu;
        if (idx < flash_data_size) {
          flash_data[idx] = curr_byte;
        }
        addr = (addr + 1) & 0x00FFFFFFu;
      }
    }
  }
  if (command == 0x9f) {
    // Read ID
    const u8 flash_id[4] = {0xCA, 0x7C, 0xA7, 0xFF};
    out_buffer = flash_id[byte_count % 4];
  }
}

// Single eval+commit step with persistent d_i (matching C++ p_d_i member
// behavior). d_i is only modified on negedge_clk; otherwise retains its previous
// value. Port of kernel_v1.metal:843.
__device__ __forceinline__ void flash_eval_commit_persistent(
  u32 clk,
  u32 csn,
  u8 d_out,
  // State (read/write):
  i32 &bit_count,
  i32 &byte_count,
  u32 &data_width,
  u32 &addr,
  u8 &curr_byte,
  u8 &command,
  u8 &out_buffer,
  u32 &prev_clk,
  u32 &prev_csn,
  u32 &last_error_cmd,
  u8 &d_i,  // persistent: only updated on negedge
  u32 &qpi,             // QSPI PSRAM mode latch (read/write)
  u32 ram_writable,     // RAM-mode config (see flash_process_byte)
  u32 enter_qpi_cmd,
  u32 quad_write_cmd,
  u32 qpi_read_dummy,
  // Flash data:
  u8 *flash_data,       // writable in RAM mode; read-only for flash
  u32 flash_data_size
  )
{
  // Edge detection
  bool posedge_clk = (clk != 0) && (prev_clk == 0);
  bool negedge_clk = (clk == 0) && (prev_clk != 0);
  bool posedge_csn = (csn != 0) && (prev_csn == 0);

  if (posedge_csn) {
    bit_count = 0;
    byte_count = 0;
    // In QPI mode the next command byte is 4-lane; the qpi latch persists
    // across CS# (mode change, not a reset).
    data_width = (qpi != 0u) ? 4u : 1u;
  } else if (posedge_clk && csn == 0) {
    if (data_width == 4) {
      curr_byte = (curr_byte << 4U) | (d_out & 0xF);
    } else {
      curr_byte = (curr_byte << 1U) | (d_out & 0x1);
    }
    out_buffer = out_buffer << data_width;
    bit_count += data_width;
    if ((u32)bit_count >= 8) {
      flash_process_byte(bit_count, byte_count, data_width, addr,
          curr_byte, command, out_buffer, last_error_cmd,
          qpi, ram_writable, enter_qpi_cmd, quad_write_cmd,
          qpi_read_dummy, flash_data, flash_data_size);
      ++byte_count;
      bit_count = 0;
    }
  } else if (negedge_clk && csn == 0) {
    // Only update d_i on negedge (matching C++ p_d_i behavior)
    if (data_width == 4) {
      d_i = (out_buffer >> 4U) & 0xFU;
    } else {
      d_i = ((out_buffer >> 7U) & 0x1U) << 1U;
    }
  }
  prev_clk = clk;
  prev_csn = csn;
}

// gpu_apply_flash_din: write FlashState.d_i → input state MISO bits. Trivial
// single-thread kernel; runs after each state_prep, before simulate. Port of
// kernel_v1.metal:904. Single block of 256 threads (thread 0 works).
__global__ void gpu_apply_flash_din(
  u32 *__restrict__ states,
  const FlashState *__restrict__ flash_state,
  const FlashDinParamsAll *__restrict__ params
  )
{
  if (threadIdx.x != 0) return;

  for (u32 f = 0; f < params->n_flashes && f < MAX_QSPI_MEMS; f++) {
    const FlashDinParams &fp = params->flashes[f];
    // Gate on selection: a deselected instance (prev_csn high) presents high-Z
    // and must not drive, else on a shared SIO bus (instances sharing d_in_pos)
    // it clobbers the selected driver. Mirrors CpuBackend::apply_flash_din_gated.
    if (flash_state[f].prev_csn != 0u) continue;
    u8 d_i = flash_state[f].d_i;
    u32 xmask_off = fp.xmask_state_offset;

    for (u32 i = 0; i < 4; i++) {
      u32 pos = fp.d_in_pos[i];
      if (pos == 0xFFFFFFFFu) continue;
      u32 word_idx = pos >> 5;
      u32 bit_mask = 1u << (pos & 31u);
      if ((d_i >> i) & 1) {
        states[word_idx] |= bit_mask;
      } else {
        states[word_idx] &= ~bit_mask;
      }
      // These MISO bits are driven (known) here, bypassing state_prep's edge
      // ops — clear their X-mask so they don't stay X-seeded (#95 phase 3).
      if (xmask_off != 0u) {
        states[xmask_off + word_idx] &= ~bit_mask;
      }
    }
  }
}

// gpu_flash_model_step: SPI flash FSM (dual-step with setup delay). Direct port
// of SpiFlashModel from spiflash_model.cc via kernel_v1.metal:943. Reads
// flash_clk/flash_csn/d_out from the OUTPUT state slot, performs the dual-step
// (delayed-CSN setup model), stores d_i in FlashState for next gpu_apply_flash_din.
// Runs TWICE per tick (after the falling-edge simulate and after the rising-edge
// simulate). Single block of 256 threads (thread 0 works).
__global__ void gpu_flash_model_step(
  u32 *__restrict__ states,
  FlashState *__restrict__ flash_state,
  const FlashModelParamsAll *__restrict__ params,
  u8 *__restrict__ flash_data
  )
{
  if (threadIdx.x != 0) return;

  for (u32 f = 0; f < params->n_flashes && f < MAX_QSPI_MEMS; f++) {
    FlashState &fs = flash_state[f];
    const FlashModelParams &fm = params->flashes[f];
    // Each instance owns its own slice of the concatenated backing store
    // (writable in RAM mode, read-only for plain flash).
    u8 *fd = flash_data + fm.data_offset;

    u32 state_size = fm.state_size;

    // Read current output state signals.
    u32 clk_word = fm.clk_out_pos >> 5;
    u32 clk_bit = fm.clk_out_pos & 31u;
    u32 clk = (states[state_size + clk_word] >> clk_bit) & 1u;

    u32 csn_word = fm.csn_out_pos >> 5;
    u32 csn_bit = fm.csn_out_pos & 31u;
    u32 csn = (states[state_size + csn_word] >> csn_bit) & 1u;

    // Read d_out nibble from output state.
    u8 d_out = 0;
    for (u32 i = 0; i < 4; i++) {
      u32 pos = fm.d_out_pos[i];
      if (pos == 0xFFFFFFFFu) continue;
      u32 w = pos >> 5;
      u32 b = pos & 31u;
      d_out |= (u8)(((states[state_size + w] >> b) & 1u) << i);
    }

    // If in reset, force d_i high + update prev values (do not step the model).
    if (fs.in_reset) {
      fs.d_i = 0x0F;
      fs.prev_clk = clk;
      fs.prev_csn = csn;
      fs.model_prev_csn = csn;
      fs.prev_d_out = d_out;
      continue;
    }

    // Load state into locals for eval_commit.
    i32 bit_count = fs.bit_count;
    i32 byte_count = fs.byte_count;
    u32 data_width = fs.data_width;
    u32 addr = fs.addr;
    u8 curr_byte = fs.curr_byte;
    u8 command = fs.command;
    u8 out_buffer = fs.out_buffer;
    u32 prev_clk = fs.prev_clk;
    u32 prev_csn = fs.prev_csn;
    u32 last_error_cmd = fs.last_error_cmd;
    u8 prev_d_out = fs.prev_d_out;

    // Dual-step for setup delay (matches old GPU sim CPU callback behavior):
    // model_prev_csn tracks the model's internal edge detection state; prev_csn
    // holds the delayed CSN (previous tick's output).
    u32 model_prev_csn = fs.model_prev_csn;

    // d_i persists across steps (matching C++ p_d_i member); only updated on
    // negedge_clk inside flash_eval_commit.
    u8 d_i = fs.d_i;

    // QSPI PSRAM mode latch (persists across steps + transactions).
    u32 qpi = fs.qpi;

    // Step 1: eval with prev_csn + prev_d_out (posedge, samples old data).
    flash_eval_commit_persistent(clk, prev_csn, prev_d_out,
        bit_count, byte_count, data_width, addr,
        curr_byte, command, out_buffer, prev_clk, model_prev_csn,
        last_error_cmd, d_i, qpi, fm.writable, fm.enter_qpi_cmd,
        fm.quad_write_cmd, fm.qpi_read_dummy, fd, fm.flash_data_size);

    // Step 2: eval with prev_csn + current d_out.
    flash_eval_commit_persistent(clk, prev_csn, d_out,
        bit_count, byte_count, data_width, addr,
        curr_byte, command, out_buffer, prev_clk, model_prev_csn,
        last_error_cmd, d_i, qpi, fm.writable, fm.enter_qpi_cmd,
        fm.quad_write_cmd, fm.qpi_read_dummy, fd, fm.flash_data_size);

    // Write state back.
    fs.bit_count = bit_count;
    fs.byte_count = byte_count;
    fs.data_width = data_width;
    fs.addr = addr;
    fs.curr_byte = curr_byte;
    fs.command = command;
    fs.out_buffer = out_buffer;
    fs.prev_clk = clk;
    fs.prev_csn = csn;  // current output csn → next tick's delayed csn
    fs.model_prev_csn = model_prev_csn;  // model's edge detection state
    fs.d_i = d_i;
    fs.prev_d_out = d_out;
    fs.last_error_cmd = last_error_cmd;
    fs.qpi = qpi;
  }
}

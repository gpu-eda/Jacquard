/* SPDX-License-Identifier: BSD-2-Clause */
#ifndef SPIFLASH_MODEL_H
#define SPIFLASH_MODEL_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct SpiFlashModel SpiFlashModel;

// Create a new flash model with given size in bytes
SpiFlashModel* spiflash_new(size_t size_bytes);

// Free the flash model
void spiflash_free(SpiFlashModel* flash);

// Load firmware into flash at given offset
// Returns number of bytes loaded, or -1 on error
int spiflash_load(SpiFlashModel* flash, const uint8_t* data, size_t len, size_t offset);

// Step the simulation
// clk: SPI clock state
// csn: chip select (active low)
// d_o: 4-bit data from controller to flash (MOSI on bit 0 in single mode)
// Returns: 4-bit data from flash to controller (MISO on bit 1 in single mode)
uint8_t spiflash_step(SpiFlashModel* flash, int clk, int csn, uint8_t d_o);

// Enable/disable verbose debug output
void spiflash_set_verbose(SpiFlashModel* flash, int verbose);

// Configure QSPI PSRAM (RAM) mode. When writable != 0 the backing store is
// writable and the following extra commands are honoured (matching the
// APS6404L-class cocotb reference qspi_psram_model.py):
//   * enter_qpi_cmd (e.g. 0x35): latch QPI mode so all subsequent command
//     bytes are sampled 4-lane. Pass -1 to disable.
//   * quad_write_cmd (e.g. 0x38): after a 24-bit address, sample data 4-lane
//     and store into the backing memory. Pass -1 to disable.
//   * qpi_read_dummy: dummy SCK cycles inserted after the address of a 0xEB
//     quad read while in QPI mode (e.g. 6). Ignored when qpi is not latched.
// Calling this with writable != 0 also zero-fills the backing store (RAM power
// on), matching the cocotb model's `bytearray(size)` init; call spiflash_load
// afterwards to preload. When writable == 0 the model is byte-identical to the
// original SPI-flash behaviour.
void spiflash_set_ram_mode(SpiFlashModel* flash, int writable,
                           int enter_qpi_cmd, int quad_write_cmd,
                           unsigned qpi_read_dummy);

// Read back a byte from the backing store (for test introspection).
uint8_t spiflash_peek(SpiFlashModel* flash, size_t addr);

// Get the current command being processed
uint8_t spiflash_get_command(SpiFlashModel* flash);

// Get the byte count in current transaction
uint32_t spiflash_get_byte_count(SpiFlashModel* flash);

// Debug: get step count
uint32_t spiflash_get_step_count(SpiFlashModel* flash);

// Debug: get posedge count
uint32_t spiflash_get_posedge_count(SpiFlashModel* flash);

// Debug: get negedge count
uint32_t spiflash_get_negedge_count(SpiFlashModel* flash);

#ifdef __cplusplus
}
#endif

#endif // SPIFLASH_MODEL_H

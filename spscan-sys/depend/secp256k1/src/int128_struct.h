#ifndef SECP256K1_INT128_STRUCT_H
#define SECP256K1_INT128_STRUCT_H

#include <stdint.h>
#include "util.h"

typedef struct {
  uint64_t lo;
  uint64_t hi;
} bwkspscan_v0_1_0_uint128;

typedef bwkspscan_v0_1_0_uint128 bwkspscan_v0_1_0_int128;

#endif

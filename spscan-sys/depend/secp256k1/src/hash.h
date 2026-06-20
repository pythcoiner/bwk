/***********************************************************************
 * Copyright (c) 2014 Pieter Wuille                                    *
 * Distributed under the MIT software license, see the accompanying    *
 * file COPYING or https://www.opensource.org/licenses/mit-license.php.*
 ***********************************************************************/

#ifndef SECP256K1_HASH_H
#define SECP256K1_HASH_H

#include <stdlib.h>
#include <stdint.h>

typedef struct {
    uint32_t s[8];
    unsigned char buf[64];
    uint64_t bytes;
} bwkspscan_v0_1_0_sha256;

static void bwkspscan_v0_1_0_sha256_initialize(bwkspscan_v0_1_0_sha256 *hash);
static void bwkspscan_v0_1_0_sha256_write(bwkspscan_v0_1_0_sha256 *hash, const unsigned char *data, size_t size);
static void bwkspscan_v0_1_0_sha256_finalize(bwkspscan_v0_1_0_sha256 *hash, unsigned char *out32);
static void bwkspscan_v0_1_0_sha256_clear(bwkspscan_v0_1_0_sha256 *hash);

typedef struct {
    bwkspscan_v0_1_0_sha256 inner, outer;
} bwkspscan_v0_1_0_hmac_sha256;

static void bwkspscan_v0_1_0_hmac_sha256_initialize(bwkspscan_v0_1_0_hmac_sha256 *hash, const unsigned char *key, size_t size);
static void bwkspscan_v0_1_0_hmac_sha256_write(bwkspscan_v0_1_0_hmac_sha256 *hash, const unsigned char *data, size_t size);
static void bwkspscan_v0_1_0_hmac_sha256_finalize(bwkspscan_v0_1_0_hmac_sha256 *hash, unsigned char *out32);
static void bwkspscan_v0_1_0_hmac_sha256_clear(bwkspscan_v0_1_0_hmac_sha256 *hash);

typedef struct {
    unsigned char v[32];
    unsigned char k[32];
    int retry;
} bwkspscan_v0_1_0_rfc6979_hmac_sha256;

static void bwkspscan_v0_1_0_rfc6979_hmac_sha256_initialize(bwkspscan_v0_1_0_rfc6979_hmac_sha256 *rng, const unsigned char *key, size_t keylen);
static void bwkspscan_v0_1_0_rfc6979_hmac_sha256_generate(bwkspscan_v0_1_0_rfc6979_hmac_sha256 *rng, unsigned char *out, size_t outlen);
static void bwkspscan_v0_1_0_rfc6979_hmac_sha256_finalize(bwkspscan_v0_1_0_rfc6979_hmac_sha256 *rng);
static void bwkspscan_v0_1_0_rfc6979_hmac_sha256_clear(bwkspscan_v0_1_0_rfc6979_hmac_sha256 *rng);

#endif /* SECP256K1_HASH_H */

/***********************************************************************
 * Copyright (c) 2014 Pieter Wuille                                    *
 * Distributed under the MIT software license, see the accompanying    *
 * file COPYING or https://www.opensource.org/licenses/mit-license.php.*
 ***********************************************************************/

#ifndef SECP256K1_SCALAR_H
#define SECP256K1_SCALAR_H

#include "util.h"

#if defined(EXHAUSTIVE_TEST_ORDER)
#include "scalar_low.h"
#elif defined(SECP256K1_WIDEMUL_INT128)
#include "scalar_4x64.h"
#elif defined(SECP256K1_WIDEMUL_INT64)
#include "scalar_8x32.h"
#else
#error "Please select wide multiplication implementation"
#endif

/** Clear a scalar to prevent the leak of sensitive data. */
static void bwkspscan_v0_1_0_scalar_clear(bwkspscan_v0_1_0_scalar *r);

/** Access bits (1 < count <= 32) from a scalar. All requested bits must belong to the same 32-bit limb. */
static uint32_t bwkspscan_v0_1_0_scalar_get_bits_limb32(const bwkspscan_v0_1_0_scalar *a, unsigned int offset, unsigned int count);

/** Access bits (1 < count <= 32) from a scalar. offset + count must be < 256. Not constant time in offset and count. */
static uint32_t bwkspscan_v0_1_0_scalar_get_bits_var(const bwkspscan_v0_1_0_scalar *a, unsigned int offset, unsigned int count);

/** Set a scalar from a big endian byte array. The scalar will be reduced modulo group order `n`.
 * In:      bin:        pointer to a 32-byte array.
 * Out:     r:          scalar to be set.
 *          overflow:   non-zero if the scalar was bigger or equal to `n` before reduction, zero otherwise (can be NULL).
 */
static void bwkspscan_v0_1_0_scalar_set_b32(bwkspscan_v0_1_0_scalar *r, const unsigned char *bin, int *overflow);

/** Set a scalar from a big endian byte array and returns 1 if it is a valid
 *  seckey and 0 otherwise. */
static int bwkspscan_v0_1_0_scalar_set_b32_seckey(bwkspscan_v0_1_0_scalar *r, const unsigned char *bin);

/** Set a scalar to an unsigned integer. */
static void bwkspscan_v0_1_0_scalar_set_int(bwkspscan_v0_1_0_scalar *r, unsigned int v);

/** Convert a scalar to a byte array. */
static void bwkspscan_v0_1_0_scalar_get_b32(unsigned char *bin, const bwkspscan_v0_1_0_scalar* a);

/** Add two scalars together (modulo the group order). Returns whether it overflowed. */
static int bwkspscan_v0_1_0_scalar_add(bwkspscan_v0_1_0_scalar *r, const bwkspscan_v0_1_0_scalar *a, const bwkspscan_v0_1_0_scalar *b);

/** Conditionally add a power of two to a scalar. The result is not allowed to overflow. */
static void bwkspscan_v0_1_0_scalar_cadd_bit(bwkspscan_v0_1_0_scalar *r, unsigned int bit, int flag);

/** Multiply two scalars (modulo the group order). */
static void bwkspscan_v0_1_0_scalar_mul(bwkspscan_v0_1_0_scalar *r, const bwkspscan_v0_1_0_scalar *a, const bwkspscan_v0_1_0_scalar *b);

/** Compute the inverse of a scalar (modulo the group order). */
static void bwkspscan_v0_1_0_scalar_inverse(bwkspscan_v0_1_0_scalar *r, const bwkspscan_v0_1_0_scalar *a);

/** Compute the inverse of a scalar (modulo the group order), without constant-time guarantee. */
static void bwkspscan_v0_1_0_scalar_inverse_var(bwkspscan_v0_1_0_scalar *r, const bwkspscan_v0_1_0_scalar *a);

/** Compute the complement of a scalar (modulo the group order). */
static void bwkspscan_v0_1_0_scalar_negate(bwkspscan_v0_1_0_scalar *r, const bwkspscan_v0_1_0_scalar *a);

/** Multiply a scalar with the multiplicative inverse of 2. */
static void bwkspscan_v0_1_0_scalar_half(bwkspscan_v0_1_0_scalar *r, const bwkspscan_v0_1_0_scalar *a);

/** Check whether a scalar equals zero. */
static int bwkspscan_v0_1_0_scalar_is_zero(const bwkspscan_v0_1_0_scalar *a);

/** Check whether a scalar equals one. */
static int bwkspscan_v0_1_0_scalar_is_one(const bwkspscan_v0_1_0_scalar *a);

/** Check whether a scalar, considered as an nonnegative integer, is even. */
static int bwkspscan_v0_1_0_scalar_is_even(const bwkspscan_v0_1_0_scalar *a);

/** Check whether a scalar is higher than the group order divided by 2. */
static int bwkspscan_v0_1_0_scalar_is_high(const bwkspscan_v0_1_0_scalar *a);

/** Conditionally negate a number, in constant time.
 * Returns -1 if the number was negated, 1 otherwise */
static int bwkspscan_v0_1_0_scalar_cond_negate(bwkspscan_v0_1_0_scalar *a, int flag);

/** Compare two scalars. */
static int bwkspscan_v0_1_0_scalar_eq(const bwkspscan_v0_1_0_scalar *a, const bwkspscan_v0_1_0_scalar *b);

/** Find r1 and r2 such that r1+r2*2^128 = k. */
static void bwkspscan_v0_1_0_scalar_split_128(bwkspscan_v0_1_0_scalar *r1, bwkspscan_v0_1_0_scalar *r2, const bwkspscan_v0_1_0_scalar *k);
/** Find r1 and r2 such that r1+r2*lambda = k, where r1 and r2 or their
 *  negations are maximum 128 bits long (see bwkspscan_v0_1_0_ge_mul_lambda). It is
 *  required that r1, r2, and k all point to different objects. */
static void bwkspscan_v0_1_0_scalar_split_lambda(bwkspscan_v0_1_0_scalar * SECP256K1_RESTRICT r1, bwkspscan_v0_1_0_scalar * SECP256K1_RESTRICT r2, const bwkspscan_v0_1_0_scalar * SECP256K1_RESTRICT k);

/** Multiply a and b (without taking the modulus!), divide by 2**shift, and round to the nearest integer. Shift must be at least 256. */
static void bwkspscan_v0_1_0_scalar_mul_shift_var(bwkspscan_v0_1_0_scalar *r, const bwkspscan_v0_1_0_scalar *a, const bwkspscan_v0_1_0_scalar *b, unsigned int shift);

/** If flag is true, set *r equal to *a; otherwise leave it. Constant-time.  Both *r and *a must be initialized.*/
static void bwkspscan_v0_1_0_scalar_cmov(bwkspscan_v0_1_0_scalar *r, const bwkspscan_v0_1_0_scalar *a, int flag);

/** Check invariants on a scalar (no-op unless VERIFY is enabled). */
static void bwkspscan_v0_1_0_scalar_verify(const bwkspscan_v0_1_0_scalar *r);
#define SECP256K1_SCALAR_VERIFY(r) bwkspscan_v0_1_0_scalar_verify(r)

#endif /* SECP256K1_SCALAR_H */

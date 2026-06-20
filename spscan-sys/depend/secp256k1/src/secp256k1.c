/***********************************************************************
 * Copyright (c) 2013-2015 Pieter Wuille                               *
 * Distributed under the MIT software license, see the accompanying    *
 * file COPYING or https://www.opensource.org/licenses/mit-license.php.*
 ***********************************************************************/

/* This is a C project. It should not be compiled with a C++ compiler,
 * and we error out if we detect one.
 *
 * We still want to be able to test the project with a C++ compiler
 * because it is still good to know if this will lead to real trouble, so
 * there is a possibility to override the check. But be warned that
 * compiling with a C++ compiler is not supported. */
#if defined(__cplusplus) && !defined(SECP256K1_CPLUSPLUS_TEST_OVERRIDE)
#error Trying to compile a C project with a C++ compiler.
#endif

#define SECP256K1_BUILD

#include "../include/secp256k1.h"
#include "../include/secp256k1_preallocated.h"

#include "assumptions.h"
#include "checkmem.h"
#include "util.h"

#include "field_impl.h"
#include "scalar_impl.h"
#include "group_impl.h"
#include "ecmult_impl.h"
#include "ecmult_const_impl.h"
#include "ecmult_gen_impl.h"
/* STRIPPED build: ECDSA sign/verify removed (unused by the SP scan path), so
 * ecdsa_impl.h is not included and ecdsa.h/ecdsa_impl.h were deleted. */
#include "eckey_impl.h"
#include "hash_impl.h"
#include "int128_impl.h"
#include "scratch_impl.h"
#include "selftest.h"
#include "hsort_impl.h"

#ifdef SECP256K1_NO_BUILD
# error "secp256k1.h processed without SECP256K1_BUILD defined while building secp256k1.c"
#endif

#define ARG_CHECK(cond) do { \
    if (EXPECT(!(cond), 0)) { \
        bwkspscan_v0_1_0_callback_call(&ctx->illegal_callback, #cond); \
        return 0; \
    } \
} while(0)

#define ARG_CHECK_VOID(cond) do { \
    if (EXPECT(!(cond), 0)) { \
        bwkspscan_v0_1_0_callback_call(&ctx->illegal_callback, #cond); \
        return; \
    } \
} while(0)

/* Note that whenever you change the context struct, you must also change the
 * context_eq function. */
struct bwkspscan_v0_1_0_context_struct {
    bwkspscan_v0_1_0_ecmult_gen_context ecmult_gen_ctx;
    bwkspscan_v0_1_0_callback illegal_callback;
    bwkspscan_v0_1_0_callback error_callback;
    int declassify;
};

static const bwkspscan_v0_1_0_context bwkspscan_v0_1_0_context_static_ = {
    { 0 },
    { bwkspscan_v0_1_0_default_illegal_callback_fn, 0 },
    { bwkspscan_v0_1_0_default_error_callback_fn, 0 },
    0
};
const bwkspscan_v0_1_0_context * const bwkspscan_v0_1_0_context_static = &bwkspscan_v0_1_0_context_static_;
const bwkspscan_v0_1_0_context * const bwkspscan_v0_1_0_context_no_precomp = &bwkspscan_v0_1_0_context_static_;

/* Helper function that determines if a context is proper, i.e., is not the static context or a copy thereof.
 *
 * This is intended for "context" functions such as bwkspscan_v0_1_0_context_clone. Functions that need specific
 * features of a context should still check for these features directly. For example, a function that needs
 * ecmult_gen should directly check for the existence of the ecmult_gen context. */
static int bwkspscan_v0_1_0_context_is_proper(const bwkspscan_v0_1_0_context* ctx) {
    return bwkspscan_v0_1_0_ecmult_gen_context_is_built(&ctx->ecmult_gen_ctx);
}

void bwkspscan_v0_1_0_selftest(void) {
    if (!bwkspscan_v0_1_0_selftest_passes()) {
        bwkspscan_v0_1_0_callback_call(&default_error_callback, "self test failed");
    }
}

size_t bwkspscan_v0_1_0_context_preallocated_size(unsigned int flags) {
    size_t ret = sizeof(bwkspscan_v0_1_0_context);
    /* A return value of 0 is reserved as an indicator for errors when we call this function internally. */
    VERIFY_CHECK(ret != 0);

    if (EXPECT((flags & SECP256K1_FLAGS_TYPE_MASK) != SECP256K1_FLAGS_TYPE_CONTEXT, 0)) {
            bwkspscan_v0_1_0_callback_call(&default_illegal_callback,
                                    "Invalid flags");
            return 0;
    }

    if (EXPECT(!SECP256K1_CHECKMEM_RUNNING() && (flags & SECP256K1_FLAGS_BIT_CONTEXT_DECLASSIFY), 0)) {
            bwkspscan_v0_1_0_callback_call(&default_illegal_callback,
                                    "Declassify flag requires running with memory checking");
            return 0;
    }

    return ret;
}

size_t bwkspscan_v0_1_0_context_preallocated_clone_size(const bwkspscan_v0_1_0_context* ctx) {
    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(bwkspscan_v0_1_0_context_is_proper(ctx));
    return sizeof(bwkspscan_v0_1_0_context);
}

bwkspscan_v0_1_0_context* bwkspscan_v0_1_0_context_preallocated_create(void* prealloc, unsigned int flags) {
    size_t prealloc_size;
    bwkspscan_v0_1_0_context* ret;

    bwkspscan_v0_1_0_selftest();

    prealloc_size = bwkspscan_v0_1_0_context_preallocated_size(flags);
    if (prealloc_size == 0) {
        return NULL;
    }
    VERIFY_CHECK(prealloc != NULL);
    ret = (bwkspscan_v0_1_0_context*)prealloc;
    ret->illegal_callback = default_illegal_callback;
    ret->error_callback = default_error_callback;

    /* Flags have been checked by bwkspscan_v0_1_0_context_preallocated_size. */
    VERIFY_CHECK((flags & SECP256K1_FLAGS_TYPE_MASK) == SECP256K1_FLAGS_TYPE_CONTEXT);
    bwkspscan_v0_1_0_ecmult_gen_context_build(&ret->ecmult_gen_ctx);
    ret->declassify = !!(flags & SECP256K1_FLAGS_BIT_CONTEXT_DECLASSIFY);

    return ret;
}

bwkspscan_v0_1_0_context* bwkspscan_v0_1_0_context_preallocated_clone(const bwkspscan_v0_1_0_context* ctx, void* prealloc) {
    bwkspscan_v0_1_0_context* ret;
    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(prealloc != NULL);
    ARG_CHECK(bwkspscan_v0_1_0_context_is_proper(ctx));

    ret = (bwkspscan_v0_1_0_context*)prealloc;
    *ret = *ctx;
    return ret;
}

void bwkspscan_v0_1_0_context_preallocated_destroy(bwkspscan_v0_1_0_context* ctx) {
    ARG_CHECK_VOID(ctx == NULL || bwkspscan_v0_1_0_context_is_proper(ctx));

    /* Defined as noop */
    if (ctx == NULL) {
        return;
    }

    bwkspscan_v0_1_0_ecmult_gen_context_clear(&ctx->ecmult_gen_ctx);
}

void bwkspscan_v0_1_0_context_set_illegal_callback(bwkspscan_v0_1_0_context* ctx, void (*fun)(const char* message, void* data), const void* data) {
    /* We compare pointers instead of checking bwkspscan_v0_1_0_context_is_proper() here
       because setting callbacks is allowed on *copies* of the static context:
       it's harmless and makes testing easier. */
    ARG_CHECK_VOID(ctx != bwkspscan_v0_1_0_context_static);
    if (fun == NULL) {
        fun = bwkspscan_v0_1_0_default_illegal_callback_fn;
    }
    ctx->illegal_callback.fn = fun;
    ctx->illegal_callback.data = data;
}

void bwkspscan_v0_1_0_context_set_error_callback(bwkspscan_v0_1_0_context* ctx, void (*fun)(const char* message, void* data), const void* data) {
    /* We compare pointers instead of checking bwkspscan_v0_1_0_context_is_proper() here
       because setting callbacks is allowed on *copies* of the static context:
       it's harmless and makes testing easier. */
    ARG_CHECK_VOID(ctx != bwkspscan_v0_1_0_context_static);
    if (fun == NULL) {
        fun = bwkspscan_v0_1_0_default_error_callback_fn;
    }
    ctx->error_callback.fn = fun;
    ctx->error_callback.data = data;
}


/* Mark memory as no-longer-secret for the purpose of analysing constant-time behaviour
 *  of the software.
 */
static SECP256K1_INLINE void bwkspscan_v0_1_0_declassify(const bwkspscan_v0_1_0_context* ctx, const void *p, size_t len) {
    if (EXPECT(ctx->declassify, 0)) SECP256K1_CHECKMEM_DEFINE(p, len);
}

static int bwkspscan_v0_1_0_pubkey_load(const bwkspscan_v0_1_0_context* ctx, bwkspscan_v0_1_0_ge* ge, const bwkspscan_v0_1_0_pubkey* pubkey) {
    bwkspscan_v0_1_0_ge_from_bytes(ge, pubkey->data);
    ARG_CHECK(!bwkspscan_v0_1_0_fe_is_zero(&ge->x));
    return 1;
}

static void bwkspscan_v0_1_0_pubkey_save(bwkspscan_v0_1_0_pubkey* pubkey, bwkspscan_v0_1_0_ge* ge) {
    bwkspscan_v0_1_0_ge_to_bytes(pubkey->data, ge);
}

int bwkspscan_v0_1_0_ec_pubkey_parse(const bwkspscan_v0_1_0_context* ctx, bwkspscan_v0_1_0_pubkey* pubkey, const unsigned char *input, size_t inputlen) {
    bwkspscan_v0_1_0_ge Q;

    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(pubkey != NULL);
    memset(pubkey, 0, sizeof(*pubkey));
    ARG_CHECK(input != NULL);
    if (!bwkspscan_v0_1_0_eckey_pubkey_parse(&Q, input, inputlen)) {
        return 0;
    }
    if (!bwkspscan_v0_1_0_ge_is_in_correct_subgroup(&Q)) {
        return 0;
    }
    bwkspscan_v0_1_0_pubkey_save(pubkey, &Q);
    bwkspscan_v0_1_0_ge_clear(&Q);
    return 1;
}

int bwkspscan_v0_1_0_ec_pubkey_serialize(const bwkspscan_v0_1_0_context* ctx, unsigned char *output, size_t *outputlen, const bwkspscan_v0_1_0_pubkey* pubkey, unsigned int flags) {
    bwkspscan_v0_1_0_ge Q;
    size_t len;
    int ret = 0;

    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(outputlen != NULL);
    ARG_CHECK(*outputlen >= ((flags & SECP256K1_FLAGS_BIT_COMPRESSION) ? 33u : 65u));
    len = *outputlen;
    *outputlen = 0;
    ARG_CHECK(output != NULL);
    memset(output, 0, len);
    ARG_CHECK(pubkey != NULL);
    ARG_CHECK((flags & SECP256K1_FLAGS_TYPE_MASK) == SECP256K1_FLAGS_TYPE_COMPRESSION);
    if (bwkspscan_v0_1_0_pubkey_load(ctx, &Q, pubkey)) {
        ret = bwkspscan_v0_1_0_eckey_pubkey_serialize(&Q, output, &len, !!(flags & SECP256K1_FLAGS_BIT_COMPRESSION));
        if (ret) {
            *outputlen = len;
        }
    }
    return ret;
}

int bwkspscan_v0_1_0_ec_pubkey_cmp(const bwkspscan_v0_1_0_context* ctx, const bwkspscan_v0_1_0_pubkey* pubkey0, const bwkspscan_v0_1_0_pubkey* pubkey1) {
    unsigned char out[2][33];
    const bwkspscan_v0_1_0_pubkey* pk[2];
    int i;

    VERIFY_CHECK(ctx != NULL);
    pk[0] = pubkey0; pk[1] = pubkey1;
    for (i = 0; i < 2; i++) {
        size_t out_size = sizeof(out[i]);
        /* If the public key is NULL or invalid, ec_pubkey_serialize will call
         * the illegal_callback and return 0. In that case we will serialize the
         * key as all zeros which is less than any valid public key. This
         * results in consistent comparisons even if NULL or invalid pubkeys are
         * involved and prevents edge cases such as sorting algorithms that use
         * this function and do not terminate as a result. */
        if (!bwkspscan_v0_1_0_ec_pubkey_serialize(ctx, out[i], &out_size, pk[i], SECP256K1_EC_COMPRESSED)) {
            /* Note that ec_pubkey_serialize should already set the output to
             * zero in that case, but it's not guaranteed by the API, we can't
             * test it and writing a VERIFY_CHECK is more complex than
             * explicitly memsetting (again). */
            memset(out[i], 0, sizeof(out[i]));
        }
    }
    return bwkspscan_v0_1_0_memcmp_var(out[0], out[1], sizeof(out[0]));
}

static int bwkspscan_v0_1_0_ec_pubkey_sort_cmp(const void* pk1, const void* pk2, void *ctx) {
    return bwkspscan_v0_1_0_ec_pubkey_cmp((bwkspscan_v0_1_0_context *)ctx,
                                     *(bwkspscan_v0_1_0_pubkey **)pk1,
                                     *(bwkspscan_v0_1_0_pubkey **)pk2);
}

int bwkspscan_v0_1_0_ec_pubkey_sort(const bwkspscan_v0_1_0_context* ctx, const bwkspscan_v0_1_0_pubkey **pubkeys, size_t n_pubkeys) {
    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(pubkeys != NULL);

    /* Suppress wrong warning (fixed in MSVC 19.33) */
    #if defined(_MSC_VER) && (_MSC_VER < 1933)
    #pragma warning(push)
    #pragma warning(disable: 4090)
    #endif

    /* Casting away const is fine because neither bwkspscan_v0_1_0_hsort nor
     * bwkspscan_v0_1_0_ec_pubkey_sort_cmp modify the data pointed to by the cmp_data
     * argument. */
    bwkspscan_v0_1_0_hsort(pubkeys, n_pubkeys, sizeof(*pubkeys), bwkspscan_v0_1_0_ec_pubkey_sort_cmp, (void *)ctx);

    #if defined(_MSC_VER) && (_MSC_VER < 1933)
    #pragma warning(pop)
    #endif

    return 1;
}

int bwkspscan_v0_1_0_ec_seckey_verify(const bwkspscan_v0_1_0_context* ctx, const unsigned char *seckey) {
    bwkspscan_v0_1_0_scalar sec;
    int ret;
    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(seckey != NULL);

    ret = bwkspscan_v0_1_0_scalar_set_b32_seckey(&sec, seckey);
    bwkspscan_v0_1_0_scalar_clear(&sec);
    return ret;
}

static int bwkspscan_v0_1_0_ec_pubkey_create_helper(const bwkspscan_v0_1_0_ecmult_gen_context *ecmult_gen_ctx, bwkspscan_v0_1_0_scalar *seckey_scalar, bwkspscan_v0_1_0_ge *p, const unsigned char *seckey) {
    bwkspscan_v0_1_0_gej pj;
    int ret;

    ret = bwkspscan_v0_1_0_scalar_set_b32_seckey(seckey_scalar, seckey);
    bwkspscan_v0_1_0_scalar_cmov(seckey_scalar, &bwkspscan_v0_1_0_scalar_one, !ret);

    bwkspscan_v0_1_0_ecmult_gen(ecmult_gen_ctx, &pj, seckey_scalar);
    bwkspscan_v0_1_0_ge_set_gej(p, &pj);
    bwkspscan_v0_1_0_gej_clear(&pj);
    return ret;
}

int bwkspscan_v0_1_0_ec_pubkey_create(const bwkspscan_v0_1_0_context* ctx, bwkspscan_v0_1_0_pubkey *pubkey, const unsigned char *seckey) {
    bwkspscan_v0_1_0_ge p;
    bwkspscan_v0_1_0_scalar seckey_scalar;
    int ret = 0;
    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(pubkey != NULL);
    memset(pubkey, 0, sizeof(*pubkey));
    ARG_CHECK(bwkspscan_v0_1_0_ecmult_gen_context_is_built(&ctx->ecmult_gen_ctx));
    ARG_CHECK(seckey != NULL);

    ret = bwkspscan_v0_1_0_ec_pubkey_create_helper(&ctx->ecmult_gen_ctx, &seckey_scalar, &p, seckey);
    bwkspscan_v0_1_0_pubkey_save(pubkey, &p);
    bwkspscan_v0_1_0_memczero(pubkey, sizeof(*pubkey), !ret);

    bwkspscan_v0_1_0_scalar_clear(&seckey_scalar);
    return ret;
}

int bwkspscan_v0_1_0_ec_seckey_negate(const bwkspscan_v0_1_0_context* ctx, unsigned char *seckey) {
    bwkspscan_v0_1_0_scalar sec;
    int ret = 0;
    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(seckey != NULL);

    ret = bwkspscan_v0_1_0_scalar_set_b32_seckey(&sec, seckey);
    bwkspscan_v0_1_0_scalar_cmov(&sec, &bwkspscan_v0_1_0_scalar_zero, !ret);
    bwkspscan_v0_1_0_scalar_negate(&sec, &sec);
    bwkspscan_v0_1_0_scalar_get_b32(seckey, &sec);

    bwkspscan_v0_1_0_scalar_clear(&sec);
    return ret;
}

int bwkspscan_v0_1_0_ec_pubkey_negate(const bwkspscan_v0_1_0_context* ctx, bwkspscan_v0_1_0_pubkey *pubkey) {
    int ret = 0;
    bwkspscan_v0_1_0_ge p;
    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(pubkey != NULL);

    ret = bwkspscan_v0_1_0_pubkey_load(ctx, &p, pubkey);
    memset(pubkey, 0, sizeof(*pubkey));
    if (ret) {
        bwkspscan_v0_1_0_ge_neg(&p, &p);
        bwkspscan_v0_1_0_pubkey_save(pubkey, &p);
    }
    return ret;
}


static int bwkspscan_v0_1_0_ec_seckey_tweak_add_helper(bwkspscan_v0_1_0_scalar *sec, const unsigned char *tweak32) {
    bwkspscan_v0_1_0_scalar term;
    int overflow = 0;
    int ret = 0;

    bwkspscan_v0_1_0_scalar_set_b32(&term, tweak32, &overflow);
    ret = (!overflow) & bwkspscan_v0_1_0_eckey_privkey_tweak_add(sec, &term);
    bwkspscan_v0_1_0_scalar_clear(&term);
    return ret;
}

int bwkspscan_v0_1_0_ec_seckey_tweak_add(const bwkspscan_v0_1_0_context* ctx, unsigned char *seckey, const unsigned char *tweak32) {
    bwkspscan_v0_1_0_scalar sec;
    int ret = 0;
    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(seckey != NULL);
    ARG_CHECK(tweak32 != NULL);

    ret = bwkspscan_v0_1_0_scalar_set_b32_seckey(&sec, seckey);
    ret &= bwkspscan_v0_1_0_ec_seckey_tweak_add_helper(&sec, tweak32);
    bwkspscan_v0_1_0_scalar_cmov(&sec, &bwkspscan_v0_1_0_scalar_zero, !ret);
    bwkspscan_v0_1_0_scalar_get_b32(seckey, &sec);

    bwkspscan_v0_1_0_scalar_clear(&sec);
    return ret;
}

static int bwkspscan_v0_1_0_ec_pubkey_tweak_add_helper(bwkspscan_v0_1_0_ge *p, const unsigned char *tweak32) {
    bwkspscan_v0_1_0_scalar term;
    int overflow = 0;
    bwkspscan_v0_1_0_scalar_set_b32(&term, tweak32, &overflow);
    return !overflow && bwkspscan_v0_1_0_eckey_pubkey_tweak_add(p, &term);
}

int bwkspscan_v0_1_0_ec_pubkey_tweak_add(const bwkspscan_v0_1_0_context* ctx, bwkspscan_v0_1_0_pubkey *pubkey, const unsigned char *tweak32) {
    bwkspscan_v0_1_0_ge p;
    int ret = 0;
    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(pubkey != NULL);
    ARG_CHECK(tweak32 != NULL);

    ret = bwkspscan_v0_1_0_pubkey_load(ctx, &p, pubkey);
    memset(pubkey, 0, sizeof(*pubkey));
    ret = ret && bwkspscan_v0_1_0_ec_pubkey_tweak_add_helper(&p, tweak32);
    if (ret) {
        bwkspscan_v0_1_0_pubkey_save(pubkey, &p);
    }

    return ret;
}

int bwkspscan_v0_1_0_ec_seckey_tweak_mul(const bwkspscan_v0_1_0_context* ctx, unsigned char *seckey, const unsigned char *tweak32) {
    bwkspscan_v0_1_0_scalar factor;
    bwkspscan_v0_1_0_scalar sec;
    int ret = 0;
    int overflow = 0;
    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(seckey != NULL);
    ARG_CHECK(tweak32 != NULL);

    bwkspscan_v0_1_0_scalar_set_b32(&factor, tweak32, &overflow);
    ret = bwkspscan_v0_1_0_scalar_set_b32_seckey(&sec, seckey);
    ret &= (!overflow) & bwkspscan_v0_1_0_eckey_privkey_tweak_mul(&sec, &factor);
    bwkspscan_v0_1_0_scalar_cmov(&sec, &bwkspscan_v0_1_0_scalar_zero, !ret);
    bwkspscan_v0_1_0_scalar_get_b32(seckey, &sec);

    bwkspscan_v0_1_0_scalar_clear(&sec);
    bwkspscan_v0_1_0_scalar_clear(&factor);
    return ret;
}

int bwkspscan_v0_1_0_ec_pubkey_tweak_mul(const bwkspscan_v0_1_0_context* ctx, bwkspscan_v0_1_0_pubkey *pubkey, const unsigned char *tweak32) {
    bwkspscan_v0_1_0_ge p;
    bwkspscan_v0_1_0_scalar factor;
    int ret = 0;
    int overflow = 0;
    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(pubkey != NULL);
    ARG_CHECK(tweak32 != NULL);

    bwkspscan_v0_1_0_scalar_set_b32(&factor, tweak32, &overflow);
    ret = !overflow && bwkspscan_v0_1_0_pubkey_load(ctx, &p, pubkey);
    memset(pubkey, 0, sizeof(*pubkey));
    if (ret) {
        if (bwkspscan_v0_1_0_eckey_pubkey_tweak_mul(&p, &factor)) {
            bwkspscan_v0_1_0_pubkey_save(pubkey, &p);
        } else {
            ret = 0;
        }
    }

    return ret;
}

int bwkspscan_v0_1_0_context_randomize(bwkspscan_v0_1_0_context* ctx, const unsigned char *seed32) {
    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(bwkspscan_v0_1_0_context_is_proper(ctx));

    if (bwkspscan_v0_1_0_ecmult_gen_context_is_built(&ctx->ecmult_gen_ctx)) {
        bwkspscan_v0_1_0_ecmult_gen_blind(&ctx->ecmult_gen_ctx, seed32);
    }
    return 1;
}

int bwkspscan_v0_1_0_ec_pubkey_combine(const bwkspscan_v0_1_0_context* ctx, bwkspscan_v0_1_0_pubkey *pubnonce, const bwkspscan_v0_1_0_pubkey * const *pubnonces, size_t n) {
    size_t i;
    bwkspscan_v0_1_0_gej Qj;
    bwkspscan_v0_1_0_ge Q;

    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(pubnonce != NULL);
    memset(pubnonce, 0, sizeof(*pubnonce));
    ARG_CHECK(n >= 1);
    ARG_CHECK(pubnonces != NULL);

    bwkspscan_v0_1_0_gej_set_infinity(&Qj);

    for (i = 0; i < n; i++) {
        ARG_CHECK(pubnonces[i] != NULL);
        bwkspscan_v0_1_0_pubkey_load(ctx, &Q, pubnonces[i]);
        bwkspscan_v0_1_0_gej_add_ge(&Qj, &Qj, &Q);
    }
    if (bwkspscan_v0_1_0_gej_is_infinity(&Qj)) {
        return 0;
    }
    bwkspscan_v0_1_0_ge_set_gej(&Q, &Qj);
    bwkspscan_v0_1_0_pubkey_save(pubnonce, &Q);
    return 1;
}

int bwkspscan_v0_1_0_tagged_sha256(const bwkspscan_v0_1_0_context* ctx, unsigned char *hash32, const unsigned char *tag, size_t taglen, const unsigned char *msg, size_t msglen) {
    bwkspscan_v0_1_0_sha256 sha;
    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(hash32 != NULL);
    ARG_CHECK(tag != NULL);
    ARG_CHECK(msg != NULL);

    bwkspscan_v0_1_0_sha256_initialize_tagged(&sha, tag, taglen);
    bwkspscan_v0_1_0_sha256_write(&sha, msg, msglen);
    bwkspscan_v0_1_0_sha256_finalize(&sha, hash32);
    bwkspscan_v0_1_0_sha256_clear(&sha);
    return 1;
}

/* STRIPPED build: only extrakeys + silentpayments are vendored. The
 * ecdh/recovery/schnorrsig/musig/ellswift module dirs were deleted. */
#ifdef ENABLE_MODULE_EXTRAKEYS
# include "modules/extrakeys/main_impl.h"
#endif

#ifdef ENABLE_MODULE_SILENTPAYMENTS
# include "modules/silentpayments/main_impl.h"
#endif

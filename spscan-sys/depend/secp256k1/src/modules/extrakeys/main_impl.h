/***********************************************************************
 * Copyright (c) 2020 Jonas Nick                                       *
 * Distributed under the MIT software license, see the accompanying    *
 * file COPYING or https://www.opensource.org/licenses/mit-license.php.*
 ***********************************************************************/

#ifndef SECP256K1_MODULE_EXTRAKEYS_MAIN_H
#define SECP256K1_MODULE_EXTRAKEYS_MAIN_H

#include "../../../include/secp256k1.h"
#include "../../../include/secp256k1_extrakeys.h"
#include "../../util.h"

static SECP256K1_INLINE int bwkspscan_v0_1_0_xonly_pubkey_load(const bwkspscan_v0_1_0_context* ctx, bwkspscan_v0_1_0_ge *ge, const bwkspscan_v0_1_0_xonly_pubkey *pubkey) {
    return bwkspscan_v0_1_0_pubkey_load(ctx, ge, (const bwkspscan_v0_1_0_pubkey *) pubkey);
}

static SECP256K1_INLINE void bwkspscan_v0_1_0_xonly_pubkey_save(bwkspscan_v0_1_0_xonly_pubkey *pubkey, bwkspscan_v0_1_0_ge *ge) {
    bwkspscan_v0_1_0_pubkey_save((bwkspscan_v0_1_0_pubkey *) pubkey, ge);
}

int bwkspscan_v0_1_0_xonly_pubkey_parse(const bwkspscan_v0_1_0_context* ctx, bwkspscan_v0_1_0_xonly_pubkey *pubkey, const unsigned char *input32) {
    bwkspscan_v0_1_0_ge pk;
    bwkspscan_v0_1_0_fe x;

    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(pubkey != NULL);
    memset(pubkey, 0, sizeof(*pubkey));
    ARG_CHECK(input32 != NULL);

    if (!bwkspscan_v0_1_0_fe_set_b32_limit(&x, input32)) {
        return 0;
    }
    if (!bwkspscan_v0_1_0_ge_set_xo_var(&pk, &x, 0)) {
        return 0;
    }
    if (!bwkspscan_v0_1_0_ge_is_in_correct_subgroup(&pk)) {
        return 0;
    }
    bwkspscan_v0_1_0_xonly_pubkey_save(pubkey, &pk);
    return 1;
}

int bwkspscan_v0_1_0_xonly_pubkey_serialize(const bwkspscan_v0_1_0_context* ctx, unsigned char *output32, const bwkspscan_v0_1_0_xonly_pubkey *pubkey) {
    bwkspscan_v0_1_0_ge pk;

    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(output32 != NULL);
    memset(output32, 0, 32);
    ARG_CHECK(pubkey != NULL);

    if (!bwkspscan_v0_1_0_xonly_pubkey_load(ctx, &pk, pubkey)) {
        return 0;
    }
    bwkspscan_v0_1_0_fe_get_b32(output32, &pk.x);
    return 1;
}

int bwkspscan_v0_1_0_xonly_pubkey_cmp(const bwkspscan_v0_1_0_context* ctx, const bwkspscan_v0_1_0_xonly_pubkey* pk0, const bwkspscan_v0_1_0_xonly_pubkey* pk1) {
    unsigned char out[2][32];
    const bwkspscan_v0_1_0_xonly_pubkey* pk[2];
    int i;

    VERIFY_CHECK(ctx != NULL);
    pk[0] = pk0; pk[1] = pk1;
    for (i = 0; i < 2; i++) {
        /* If the public key is NULL or invalid, xonly_pubkey_serialize will
         * call the illegal_callback and return 0. In that case we will
         * serialize the key as all zeros which is less than any valid public
         * key. This results in consistent comparisons even if NULL or invalid
         * pubkeys are involved and prevents edge cases such as sorting
         * algorithms that use this function and do not terminate as a
         * result. */
        if (!bwkspscan_v0_1_0_xonly_pubkey_serialize(ctx, out[i], pk[i])) {
            /* Note that xonly_pubkey_serialize should already set the output to
             * zero in that case, but it's not guaranteed by the API, we can't
             * test it and writing a VERIFY_CHECK is more complex than
             * explicitly memsetting (again). */
            memset(out[i], 0, sizeof(out[i]));
        }
    }
    return bwkspscan_v0_1_0_memcmp_var(out[0], out[1], sizeof(out[1]));
}

/** Keeps a group element as is if it has an even Y and otherwise negates it.
 *  y_parity is set to 0 in the former case and to 1 in the latter case.
 *  Requires that the coordinates of r are normalized. */
static int bwkspscan_v0_1_0_extrakeys_ge_even_y(bwkspscan_v0_1_0_ge *r) {
    int y_parity = 0;
    VERIFY_CHECK(!bwkspscan_v0_1_0_ge_is_infinity(r));

    if (bwkspscan_v0_1_0_fe_is_odd(&r->y)) {
        bwkspscan_v0_1_0_fe_negate(&r->y, &r->y, 1);
        y_parity = 1;
    }
    return y_parity;
}

int bwkspscan_v0_1_0_xonly_pubkey_from_pubkey(const bwkspscan_v0_1_0_context* ctx, bwkspscan_v0_1_0_xonly_pubkey *xonly_pubkey, int *pk_parity, const bwkspscan_v0_1_0_pubkey *pubkey) {
    bwkspscan_v0_1_0_ge pk;
    int tmp;

    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(xonly_pubkey != NULL);
    ARG_CHECK(pubkey != NULL);

    if (!bwkspscan_v0_1_0_pubkey_load(ctx, &pk, pubkey)) {
        return 0;
    }
    tmp = bwkspscan_v0_1_0_extrakeys_ge_even_y(&pk);
    if (pk_parity != NULL) {
        *pk_parity = tmp;
    }
    bwkspscan_v0_1_0_xonly_pubkey_save(xonly_pubkey, &pk);
    return 1;
}

int bwkspscan_v0_1_0_xonly_pubkey_tweak_add(const bwkspscan_v0_1_0_context* ctx, bwkspscan_v0_1_0_pubkey *output_pubkey, const bwkspscan_v0_1_0_xonly_pubkey *internal_pubkey, const unsigned char *tweak32) {
    bwkspscan_v0_1_0_ge pk;

    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(output_pubkey != NULL);
    memset(output_pubkey, 0, sizeof(*output_pubkey));
    ARG_CHECK(internal_pubkey != NULL);
    ARG_CHECK(tweak32 != NULL);

    if (!bwkspscan_v0_1_0_xonly_pubkey_load(ctx, &pk, internal_pubkey)
        || !bwkspscan_v0_1_0_ec_pubkey_tweak_add_helper(&pk, tweak32)) {
        return 0;
    }
    bwkspscan_v0_1_0_pubkey_save(output_pubkey, &pk);
    return 1;
}

int bwkspscan_v0_1_0_xonly_pubkey_tweak_add_check(const bwkspscan_v0_1_0_context* ctx, const unsigned char *tweaked_pubkey32, int tweaked_pk_parity, const bwkspscan_v0_1_0_xonly_pubkey *internal_pubkey, const unsigned char *tweak32) {
    bwkspscan_v0_1_0_ge pk;
    unsigned char pk_expected32[32];

    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(internal_pubkey != NULL);
    ARG_CHECK(tweaked_pubkey32 != NULL);
    ARG_CHECK(tweak32 != NULL);

    if (!bwkspscan_v0_1_0_xonly_pubkey_load(ctx, &pk, internal_pubkey)
        || !bwkspscan_v0_1_0_ec_pubkey_tweak_add_helper(&pk, tweak32)) {
        return 0;
    }
    bwkspscan_v0_1_0_fe_normalize_var(&pk.x);
    bwkspscan_v0_1_0_fe_normalize_var(&pk.y);
    bwkspscan_v0_1_0_fe_get_b32(pk_expected32, &pk.x);

    return bwkspscan_v0_1_0_memcmp_var(&pk_expected32, tweaked_pubkey32, 32) == 0
            && bwkspscan_v0_1_0_fe_is_odd(&pk.y) == tweaked_pk_parity;
}

static void bwkspscan_v0_1_0_keypair_save(bwkspscan_v0_1_0_keypair *keypair, const bwkspscan_v0_1_0_scalar *sk, bwkspscan_v0_1_0_ge *pk) {
    bwkspscan_v0_1_0_scalar_get_b32(&keypair->data[0], sk);
    bwkspscan_v0_1_0_pubkey_save((bwkspscan_v0_1_0_pubkey *)&keypair->data[32], pk);
}


static int bwkspscan_v0_1_0_keypair_seckey_load(const bwkspscan_v0_1_0_context* ctx, bwkspscan_v0_1_0_scalar *sk, const bwkspscan_v0_1_0_keypair *keypair) {
    int ret;

    ret = bwkspscan_v0_1_0_scalar_set_b32_seckey(sk, &keypair->data[0]);
    /* We can declassify ret here because sk is only zero if a keypair function
     * failed (which zeroes the keypair) and its return value is ignored. */
    bwkspscan_v0_1_0_declassify(ctx, &ret, sizeof(ret));
    ARG_CHECK(ret);
    return ret;
}

/* Load a keypair into pk and sk (if non-NULL). This function declassifies pk
 * and ARG_CHECKs that the keypair is not invalid. It always initializes sk and
 * pk with dummy values. */
static int bwkspscan_v0_1_0_keypair_load(const bwkspscan_v0_1_0_context* ctx, bwkspscan_v0_1_0_scalar *sk, bwkspscan_v0_1_0_ge *pk, const bwkspscan_v0_1_0_keypair *keypair) {
    int ret;
    const bwkspscan_v0_1_0_pubkey *pubkey = (const bwkspscan_v0_1_0_pubkey *)&keypair->data[32];

    /* Need to declassify the pubkey because pubkey_load ARG_CHECKs if it's
     * invalid. */
    bwkspscan_v0_1_0_declassify(ctx, pubkey, sizeof(*pubkey));
    ret = bwkspscan_v0_1_0_pubkey_load(ctx, pk, pubkey);
    if (sk != NULL) {
        ret = ret && bwkspscan_v0_1_0_keypair_seckey_load(ctx, sk, keypair);
    }
    if (!ret) {
        *pk = bwkspscan_v0_1_0_ge_const_g;
        if (sk != NULL) {
            *sk = bwkspscan_v0_1_0_scalar_one;
        }
    }
    return ret;
}

int bwkspscan_v0_1_0_keypair_create(const bwkspscan_v0_1_0_context* ctx, bwkspscan_v0_1_0_keypair *keypair, const unsigned char *seckey32) {
    bwkspscan_v0_1_0_scalar sk;
    bwkspscan_v0_1_0_ge pk;
    int ret = 0;
    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(keypair != NULL);
    memset(keypair, 0, sizeof(*keypair));
    ARG_CHECK(bwkspscan_v0_1_0_ecmult_gen_context_is_built(&ctx->ecmult_gen_ctx));
    ARG_CHECK(seckey32 != NULL);

    ret = bwkspscan_v0_1_0_ec_pubkey_create_helper(&ctx->ecmult_gen_ctx, &sk, &pk, seckey32);
    bwkspscan_v0_1_0_keypair_save(keypair, &sk, &pk);
    bwkspscan_v0_1_0_memczero(keypair, sizeof(*keypair), !ret);

    bwkspscan_v0_1_0_scalar_clear(&sk);
    return ret;
}

int bwkspscan_v0_1_0_keypair_sec(const bwkspscan_v0_1_0_context* ctx, unsigned char *seckey, const bwkspscan_v0_1_0_keypair *keypair) {
    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(seckey != NULL);
    memset(seckey, 0, 32);
    ARG_CHECK(keypair != NULL);

    memcpy(seckey, &keypair->data[0], 32);
    return 1;
}

int bwkspscan_v0_1_0_keypair_pub(const bwkspscan_v0_1_0_context* ctx, bwkspscan_v0_1_0_pubkey *pubkey, const bwkspscan_v0_1_0_keypair *keypair) {
    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(pubkey != NULL);
    memset(pubkey, 0, sizeof(*pubkey));
    ARG_CHECK(keypair != NULL);

    memcpy(pubkey->data, &keypair->data[32], sizeof(*pubkey));
    return 1;
}

int bwkspscan_v0_1_0_keypair_xonly_pub(const bwkspscan_v0_1_0_context* ctx, bwkspscan_v0_1_0_xonly_pubkey *pubkey, int *pk_parity, const bwkspscan_v0_1_0_keypair *keypair) {
    bwkspscan_v0_1_0_ge pk;
    int tmp;

    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(pubkey != NULL);
    memset(pubkey, 0, sizeof(*pubkey));
    ARG_CHECK(keypair != NULL);

    if (!bwkspscan_v0_1_0_keypair_load(ctx, NULL, &pk, keypair)) {
        return 0;
    }
    tmp = bwkspscan_v0_1_0_extrakeys_ge_even_y(&pk);
    if (pk_parity != NULL) {
        *pk_parity = tmp;
    }
    bwkspscan_v0_1_0_xonly_pubkey_save(pubkey, &pk);

    return 1;
}

int bwkspscan_v0_1_0_keypair_xonly_tweak_add(const bwkspscan_v0_1_0_context* ctx, bwkspscan_v0_1_0_keypair *keypair, const unsigned char *tweak32) {
    bwkspscan_v0_1_0_ge pk;
    bwkspscan_v0_1_0_scalar sk;
    int y_parity;
    int ret;

    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(keypair != NULL);
    ARG_CHECK(tweak32 != NULL);

    ret = bwkspscan_v0_1_0_keypair_load(ctx, &sk, &pk, keypair);
    memset(keypair, 0, sizeof(*keypair));

    y_parity = bwkspscan_v0_1_0_extrakeys_ge_even_y(&pk);
    if (y_parity == 1) {
        bwkspscan_v0_1_0_scalar_negate(&sk, &sk);
    }

    ret &= bwkspscan_v0_1_0_ec_seckey_tweak_add_helper(&sk, tweak32);
    ret &= bwkspscan_v0_1_0_ec_pubkey_tweak_add_helper(&pk, tweak32);

    bwkspscan_v0_1_0_declassify(ctx, &ret, sizeof(ret));
    if (ret) {
        bwkspscan_v0_1_0_keypair_save(keypair, &sk, &pk);
    }

    bwkspscan_v0_1_0_scalar_clear(&sk);
    return ret;
}

#endif

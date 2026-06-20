/***********************************************************************
 * Copyright (c) 2013, 2014 Pieter Wuille                              *
 * Distributed under the MIT software license, see the accompanying    *
 * file COPYING or https://www.opensource.org/licenses/mit-license.php.*
 ***********************************************************************/

#ifndef SECP256K1_ECKEY_IMPL_H
#define SECP256K1_ECKEY_IMPL_H

#include "eckey.h"

#include "util.h"
#include "scalar.h"
#include "field.h"
#include "group.h"
#include "ecmult_gen.h"

static int bwkspscan_v0_1_0_eckey_pubkey_parse(bwkspscan_v0_1_0_ge *elem, const unsigned char *pub, size_t size) {
    if (size == 33 && (pub[0] == SECP256K1_TAG_PUBKEY_EVEN || pub[0] == SECP256K1_TAG_PUBKEY_ODD)) {
        bwkspscan_v0_1_0_fe x;
        return bwkspscan_v0_1_0_fe_set_b32_limit(&x, pub+1) && bwkspscan_v0_1_0_ge_set_xo_var(elem, &x, pub[0] == SECP256K1_TAG_PUBKEY_ODD);
    } else if (size == 65 && (pub[0] == SECP256K1_TAG_PUBKEY_UNCOMPRESSED || pub[0] == SECP256K1_TAG_PUBKEY_HYBRID_EVEN || pub[0] == SECP256K1_TAG_PUBKEY_HYBRID_ODD)) {
        bwkspscan_v0_1_0_fe x, y;
        if (!bwkspscan_v0_1_0_fe_set_b32_limit(&x, pub+1) || !bwkspscan_v0_1_0_fe_set_b32_limit(&y, pub+33)) {
            return 0;
        }
        bwkspscan_v0_1_0_ge_set_xy(elem, &x, &y);
        if ((pub[0] == SECP256K1_TAG_PUBKEY_HYBRID_EVEN || pub[0] == SECP256K1_TAG_PUBKEY_HYBRID_ODD) &&
            bwkspscan_v0_1_0_fe_is_odd(&y) != (pub[0] == SECP256K1_TAG_PUBKEY_HYBRID_ODD)) {
            return 0;
        }
        return bwkspscan_v0_1_0_ge_is_valid_var(elem);
    } else {
        return 0;
    }
}

static int bwkspscan_v0_1_0_eckey_pubkey_serialize(bwkspscan_v0_1_0_ge *elem, unsigned char *pub, size_t *size, int compressed) {
    VERIFY_CHECK(compressed == 0 || compressed == 1);

    if (bwkspscan_v0_1_0_ge_is_infinity(elem)) {
        return 0;
    }
    bwkspscan_v0_1_0_fe_normalize_var(&elem->x);
    bwkspscan_v0_1_0_fe_normalize_var(&elem->y);
    bwkspscan_v0_1_0_fe_get_b32(&pub[1], &elem->x);
    if (compressed) {
        *size = 33;
        pub[0] = bwkspscan_v0_1_0_fe_is_odd(&elem->y) ? SECP256K1_TAG_PUBKEY_ODD : SECP256K1_TAG_PUBKEY_EVEN;
    } else {
        *size = 65;
        pub[0] = SECP256K1_TAG_PUBKEY_UNCOMPRESSED;
        bwkspscan_v0_1_0_fe_get_b32(&pub[33], &elem->y);
    }
    return 1;
}

static int bwkspscan_v0_1_0_eckey_privkey_tweak_add(bwkspscan_v0_1_0_scalar *key, const bwkspscan_v0_1_0_scalar *tweak) {
    bwkspscan_v0_1_0_scalar_add(key, key, tweak);
    return !bwkspscan_v0_1_0_scalar_is_zero(key);
}

static int bwkspscan_v0_1_0_eckey_pubkey_tweak_add(bwkspscan_v0_1_0_ge *key, const bwkspscan_v0_1_0_scalar *tweak) {
    bwkspscan_v0_1_0_gej pt;
    bwkspscan_v0_1_0_gej_set_ge(&pt, key);
    bwkspscan_v0_1_0_ecmult(&pt, &pt, &bwkspscan_v0_1_0_scalar_one, tweak);

    if (bwkspscan_v0_1_0_gej_is_infinity(&pt)) {
        return 0;
    }
    bwkspscan_v0_1_0_ge_set_gej(key, &pt);
    return 1;
}

static int bwkspscan_v0_1_0_eckey_privkey_tweak_mul(bwkspscan_v0_1_0_scalar *key, const bwkspscan_v0_1_0_scalar *tweak) {
    int ret;
    ret = !bwkspscan_v0_1_0_scalar_is_zero(tweak);

    bwkspscan_v0_1_0_scalar_mul(key, key, tweak);
    return ret;
}

static int bwkspscan_v0_1_0_eckey_pubkey_tweak_mul(bwkspscan_v0_1_0_ge *key, const bwkspscan_v0_1_0_scalar *tweak) {
    bwkspscan_v0_1_0_gej pt;
    if (bwkspscan_v0_1_0_scalar_is_zero(tweak)) {
        return 0;
    }

    bwkspscan_v0_1_0_gej_set_ge(&pt, key);
    bwkspscan_v0_1_0_ecmult(&pt, &pt, tweak, &bwkspscan_v0_1_0_scalar_zero);
    bwkspscan_v0_1_0_ge_set_gej(key, &pt);
    return 1;
}

#endif /* SECP256K1_ECKEY_IMPL_H */

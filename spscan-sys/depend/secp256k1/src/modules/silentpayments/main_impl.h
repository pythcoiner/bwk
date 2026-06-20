/***********************************************************************
 * Distributed under the MIT software license, see the accompanying    *
 * file COPYING or https://www.opensource.org/licenses/mit-license.php.*
 ***********************************************************************/

#ifndef SECP256K1_MODULE_SILENTPAYMENTS_MAIN_H
#define SECP256K1_MODULE_SILENTPAYMENTS_MAIN_H

#include "../../../include/secp256k1.h"
#include "../../../include/secp256k1_extrakeys.h"
#include "../../../include/secp256k1_silentpayments.h"
#include "../../ecmult.h"

/* STRIPPED build: only the two light-client scan kernels
 * (recipient_scan_lightclient_spend_points[_batch]) and the static helpers they
 * transitively call are kept. The sender path, full-node recipient scan, public
 * data (de)serialization, labels, sorting and input-hash code were removed; see
 * depend/README. The base recipient_scan_lightclient and create_output_pubkey
 * helpers were also dropped (no kept entry point reaches them). */

static void bwkspscan_v0_1_0_silentpayments_create_shared_secret_vartime(const bwkspscan_v0_1_0_context *ctx, unsigned char *shared_secret33, const bwkspscan_v0_1_0_scalar *secret_component, const bwkspscan_v0_1_0_ge *public_component) {
    bwkspscan_v0_1_0_gej ss_j;
    bwkspscan_v0_1_0_gej a_gej;
    bwkspscan_v0_1_0_ge ss;
    bwkspscan_v0_1_0_scalar zero;
    size_t len;
    int ret;

    /* VARIABLE TIME, recipient scanning only, never signing */
    bwkspscan_v0_1_0_gej_set_ge(&a_gej, public_component);
    bwkspscan_v0_1_0_scalar_set_int(&zero, 0);
    bwkspscan_v0_1_0_ecmult(&ss_j, &a_gej, secret_component, &zero);
    bwkspscan_v0_1_0_ge_set_gej(&ss, &ss_j);
    bwkspscan_v0_1_0_declassify(ctx, &ss, sizeof(ss));
    /* This can only fail if the shared secret is the point at infinity, which should be
     * impossible at this point, considering we have already validated the public key and
     * the secret key being used
     */
    ret = bwkspscan_v0_1_0_eckey_pubkey_serialize(&ss, shared_secret33, &len, 1);
    VERIFY_CHECK(ret && len == 33);
    (void)ret;
    /* While not technically "secret" data, explicitly clear the shared secret since leaking this would allow an attacker
     * to identify the resulting transaction as a silent payments transaction and potentially link the transaction
     * back to the silent payment address
     */
    bwkspscan_v0_1_0_ge_clear(&ss);
    bwkspscan_v0_1_0_gej_clear(&ss_j);
}

/** Set hash state to the BIP340 tagged hash midstate for "BIP0352/SharedSecret". */
static void bwkspscan_v0_1_0_silentpayments_sha256_init_sharedsecret(bwkspscan_v0_1_0_sha256* hash) {
    bwkspscan_v0_1_0_sha256_initialize(hash);
    hash->s[0] = 0x88831537ul;
    hash->s[1] = 0x5127079bul;
    hash->s[2] = 0x69c2137bul;
    hash->s[3] = 0xab0303e6ul;
    hash->s[4] = 0x98fa21faul;
    hash->s[5] = 0x4a888523ul;
    hash->s[6] = 0xbd99daabul;
    hash->s[7] = 0xf25e5e0aul;

    hash->bytes = 64;
}

static void bwkspscan_v0_1_0_silentpayments_create_t_k(bwkspscan_v0_1_0_scalar *t_k_scalar, const unsigned char *shared_secret33, uint32_t k) {
    bwkspscan_v0_1_0_sha256 hash;
    unsigned char hash_ser[32];
    unsigned char k_serialized[4];
    int overflow = 0;

    /* Compute t_k = hash(shared_secret || ser_32(k))  [sha256 with tag "BIP0352/SharedSecret"] */
    bwkspscan_v0_1_0_silentpayments_sha256_init_sharedsecret(&hash);
    bwkspscan_v0_1_0_sha256_write(&hash, shared_secret33, 33);
    bwkspscan_v0_1_0_write_be32(k_serialized, k);
    bwkspscan_v0_1_0_sha256_write(&hash, k_serialized, sizeof(k_serialized));
    bwkspscan_v0_1_0_sha256_finalize(&hash, hash_ser);
    bwkspscan_v0_1_0_scalar_set_b32(t_k_scalar, hash_ser, &overflow);
    VERIFY_CHECK(!overflow);
    VERIFY_CHECK(!bwkspscan_v0_1_0_scalar_is_zero(t_k_scalar));
    /* While not technically "secret" data, explicitly clear hash_ser since leaking this would allow an attacker
     * to identify the resulting transaction as a silent payments transaction and potentially link the transaction
     * back to the silent payment address
     */
    bwkspscan_v0_1_0_memclear(hash_ser, sizeof(hash_ser));
}

/* Candidate spend points converted to affine per variable-time batch inversion.
 * Bounded so the stack stays small even for large label sets. */
#define SP_CANDIDATE_BATCH 64

int bwkspscan_v0_1_0_silentpayments_recipient_scan_lightclient_spend_points(
    const bwkspscan_v0_1_0_context *ctx,
    bwkspscan_v0_1_0_xonly_pubkey *candidate_xonly,
    size_t *n_candidates,
    const bwkspscan_v0_1_0_pubkey *combined_tweak,
    const unsigned char *scan_key32,
    const bwkspscan_v0_1_0_pubkey *spend_points,
    size_t n_spend_points
)
{
    unsigned char shared_secret33[33];
    bwkspscan_v0_1_0_ge combined_tweak_ge;
    bwkspscan_v0_1_0_scalar t0_scalar;
    bwkspscan_v0_1_0_gej t0_gej;
    bwkspscan_v0_1_0_gej candidate_gej[SP_CANDIDATE_BATCH];
    bwkspscan_v0_1_0_ge candidate_ge[SP_CANDIDATE_BATCH];
    size_t i;
    int ret;

    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(bwkspscan_v0_1_0_ecmult_gen_context_is_built(&ctx->ecmult_gen_ctx));
    ARG_CHECK(candidate_xonly != NULL);
    ARG_CHECK(n_candidates != NULL);
    ARG_CHECK(combined_tweak != NULL);
    ARG_CHECK(scan_key32 != NULL);
    ARG_CHECK(spend_points != NULL);
    ARG_CHECK(*n_candidates >= n_spend_points);

    ret = bwkspscan_v0_1_0_pubkey_load(ctx, &combined_tweak_ge, combined_tweak);
    if (!ret) {
        return 0;
    }
    /* Recompute the shared secret with the scan key in scalar form, then derive
     * t_0 and t_0 * G once for the whole batch. */
    {
        bwkspscan_v0_1_0_scalar rsk_scalar;
        ret = bwkspscan_v0_1_0_scalar_set_b32_seckey(&rsk_scalar, scan_key32);
        if (!ret) {
            bwkspscan_v0_1_0_scalar_clear(&rsk_scalar);
            return 0;
        }
        bwkspscan_v0_1_0_declassify(ctx, &rsk_scalar, sizeof(rsk_scalar));
        bwkspscan_v0_1_0_silentpayments_create_shared_secret_vartime(ctx, shared_secret33, &rsk_scalar, &combined_tweak_ge);
        bwkspscan_v0_1_0_scalar_clear(&rsk_scalar);
    }
    bwkspscan_v0_1_0_silentpayments_create_t_k(&t0_scalar, shared_secret33, 0);
    bwkspscan_v0_1_0_ecmult_gen(&ctx->ecmult_gen_ctx, &t0_gej, &t0_scalar);

    /* For each precomputed spend point P_i, candidate = P_i + t_0 * G. The whole
     * batch is brought to affine with a single variable-time inversion (Montgomery's
     * trick) instead of one constant-time inversion per candidate: scanning is
     * receiver-side over public data, so constant time is unnecessary here. Process
     * in bounded chunks so the stack stays small for large label sets. */
    for (i = 0; i < n_spend_points; i += SP_CANDIDATE_BATCH) {
        size_t chunk = n_spend_points - i;
        size_t j;
        if (chunk > SP_CANDIDATE_BATCH) {
            chunk = SP_CANDIDATE_BATCH;
        }
        for (j = 0; j < chunk; j++) {
            bwkspscan_v0_1_0_ge spend_ge;
            if (!bwkspscan_v0_1_0_pubkey_load(ctx, &spend_ge, &spend_points[i + j])) {
                bwkspscan_v0_1_0_scalar_clear(&t0_scalar);
                bwkspscan_v0_1_0_gej_clear(&t0_gej);
                bwkspscan_v0_1_0_memclear(shared_secret33, sizeof(shared_secret33));
                return 0;
            }
            bwkspscan_v0_1_0_gej_add_ge_var(&candidate_gej[j], &t0_gej, &spend_ge, NULL);
        }
        bwkspscan_v0_1_0_ge_set_all_gej_var(candidate_ge, candidate_gej, chunk);
        for (j = 0; j < chunk; j++) {
            /* Adding the hashed tweak point to a valid spend point can only yield the
             * point at infinity with negligible probability. */
            if (bwkspscan_v0_1_0_ge_is_infinity(&candidate_ge[j])) {
                bwkspscan_v0_1_0_scalar_clear(&t0_scalar);
                bwkspscan_v0_1_0_gej_clear(&t0_gej);
                bwkspscan_v0_1_0_memclear(shared_secret33, sizeof(shared_secret33));
                return 0;
            }
            bwkspscan_v0_1_0_xonly_pubkey_save(&candidate_xonly[i + j], &candidate_ge[j]);
        }
    }

    /* Explicitly clear secrets. While the shared secret and t_0 are not strictly
     * "secret", leaking them lets a third party link the transaction back to the
     * recipient address. */
    bwkspscan_v0_1_0_scalar_clear(&t0_scalar);
    bwkspscan_v0_1_0_gej_clear(&t0_gej);
    bwkspscan_v0_1_0_memclear(shared_secret33, sizeof(shared_secret33));
    *n_candidates = n_spend_points;
    return 1;
}

/* Number of tweaks processed per phased chunk. Each chunk holds one per-lane
 * gej/ge/scalar array of this length on the stack, so keep it small. */
#define SP_BATCH_TWEAK_CHUNK 32

/* Lanes processed in lockstep by the interleaved ECDH below. The fixed-scalar
 * wnaf schedule is shared across the lanes, so the latency-bound field ops of
 * the per-lane doublings and adds overlap in the CPU pipeline. Tuned on this
 * i7: 6 lanes is the sweet spot before table/register pressure costs the win. */
#define SP_ECDH_LANES 6

/* Compute r[i] = rsk * a[i] for `lanes` points (lanes <= SP_ECDH_LANES) in
 * lockstep. This is a strauss fork specialized for a single fixed scalar shared
 * by all lanes: the GLV split and wnaf of rsk are computed once, then every lane
 * keeps its own odd-multiples table and accumulator. The doubling/add schedule
 * matches bwkspscan_v0_1_0_ecmult(r, a, rsk, &zero) exactly per lane, so
 * the results are byte-identical to calling ecmult once per point. */
static void bwkspscan_v0_1_0_silentpayments_ecdh_lockstep(
    bwkspscan_v0_1_0_gej *r,
    const bwkspscan_v0_1_0_gej *a,
    const bwkspscan_v0_1_0_scalar *rsk,
    size_t lanes
) {
    bwkspscan_v0_1_0_scalar na_1, na_lam;
    int wnaf_na_1[129];
    int wnaf_na_lam[129];
    int bits_na_1, bits_na_lam, bits;
    bwkspscan_v0_1_0_ge pre_a[SP_ECDH_LANES][ECMULT_TABLE_SIZE(WINDOW_A)];
    bwkspscan_v0_1_0_fe aux[SP_ECDH_LANES][ECMULT_TABLE_SIZE(WINDOW_A)];
    bwkspscan_v0_1_0_fe Z[SP_ECDH_LANES];
    bwkspscan_v0_1_0_ge tmpa;
    int i;
    size_t k;

    /* Split rsk once and build its shared wnaf representation. */
    bwkspscan_v0_1_0_scalar_split_lambda(&na_1, &na_lam, rsk);
    bits_na_1   = bwkspscan_v0_1_0_ecmult_wnaf(wnaf_na_1,   129, &na_1,   WINDOW_A);
    bits_na_lam = bwkspscan_v0_1_0_ecmult_wnaf(wnaf_na_lam, 129, &na_lam, WINDOW_A);
    bits = bits_na_1 > bits_na_lam ? bits_na_1 : bits_na_lam;

    /* Per lane: odd-multiples table and its beta-twisted x coordinates. Each
     * lane keeps its own Z denominator, exactly as the single-point ecmult does
     * for one point with ng == NULL. */
    for (k = 0; k < lanes; k++) {
        bwkspscan_v0_1_0_gej tmp = a[k];
        bwkspscan_v0_1_0_fe_set_int(&Z[k], 1);
        bwkspscan_v0_1_0_ecmult_odd_multiples_table(ECMULT_TABLE_SIZE(WINDOW_A), pre_a[k], aux[k], &Z[k], &tmp);
        bwkspscan_v0_1_0_ge_table_set_globalz(ECMULT_TABLE_SIZE(WINDOW_A), pre_a[k], aux[k]);
        for (i = 0; i < ECMULT_TABLE_SIZE(WINDOW_A); i++) {
            bwkspscan_v0_1_0_fe_mul(&aux[k][i], &pre_a[k][i].x, &bwkspscan_v0_1_0_const_beta);
        }
        bwkspscan_v0_1_0_gej_set_infinity(&r[k]);
    }

    /* Shared doubling schedule: double all lanes back-to-back, then add each
     * lane's own contribution for this bit position. */
    for (i = bits - 1; i >= 0; i--) {
        int n;
        for (k = 0; k < lanes; k++) {
            bwkspscan_v0_1_0_gej_double_var(&r[k], &r[k], NULL);
        }
        for (k = 0; k < lanes; k++) {
            if (i < bits_na_1 && (n = wnaf_na_1[i])) {
                bwkspscan_v0_1_0_ecmult_table_get_ge(&tmpa, pre_a[k], n, WINDOW_A);
                bwkspscan_v0_1_0_gej_add_ge_var(&r[k], &r[k], &tmpa, NULL);
            }
            if (i < bits_na_lam && (n = wnaf_na_lam[i])) {
                bwkspscan_v0_1_0_ecmult_table_get_ge_lambda(&tmpa, pre_a[k], aux[k], n, WINDOW_A);
                bwkspscan_v0_1_0_gej_add_ge_var(&r[k], &r[k], &tmpa, NULL);
            }
        }
    }

    for (k = 0; k < lanes; k++) {
        if (!r[k].infinity) {
            bwkspscan_v0_1_0_fe_mul(&r[k].z, &r[k].z, &Z[k]);
        }
    }
}

int bwkspscan_v0_1_0_silentpayments_recipient_scan_lightclient_spend_points_batch(
    const bwkspscan_v0_1_0_context *ctx,
    bwkspscan_v0_1_0_xonly_pubkey *out_xonly,
    size_t *n_out,
    const bwkspscan_v0_1_0_pubkey *tweaks,
    size_t n_tweaks,
    const unsigned char *scan_key32,
    const bwkspscan_v0_1_0_pubkey *spend_points,
    size_t n_spend_points
)
{
    unsigned char shared_secret33[33];
    bwkspscan_v0_1_0_scalar rsk_scalar;
    /* Per-lane chunk buffers, laid out as contiguous arrays so a later pass can
     * interleave/vectorize across lanes. */
    bwkspscan_v0_1_0_gej ss_j[SP_BATCH_TWEAK_CHUNK];
    bwkspscan_v0_1_0_ge ss_ge[SP_BATCH_TWEAK_CHUNK];
    bwkspscan_v0_1_0_scalar t0_scalar[SP_BATCH_TWEAK_CHUNK];
    bwkspscan_v0_1_0_gej t0_gej[SP_BATCH_TWEAK_CHUNK];
    /* Bounded candidate scratch, reused per Phase-E sub-chunk. */
    bwkspscan_v0_1_0_gej candidate_gej[SP_CANDIDATE_BATCH];
    bwkspscan_v0_1_0_ge candidate_ge[SP_CANDIDATE_BATCH];
    size_t base;
    size_t t, s;

    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(bwkspscan_v0_1_0_ecmult_gen_context_is_built(&ctx->ecmult_gen_ctx));
    ARG_CHECK(out_xonly != NULL);
    ARG_CHECK(n_out != NULL);
    ARG_CHECK(tweaks != NULL);
    ARG_CHECK(n_tweaks > 0);
    ARG_CHECK(scan_key32 != NULL);
    ARG_CHECK(spend_points != NULL);
    ARG_CHECK(n_spend_points > 0);
    ARG_CHECK(*n_out >= n_tweaks * n_spend_points);

    /* Load the scan key once for the whole batch. */
    if (!bwkspscan_v0_1_0_scalar_set_b32_seckey(&rsk_scalar, scan_key32)) {
        bwkspscan_v0_1_0_scalar_clear(&rsk_scalar);
        return 0;
    }
    bwkspscan_v0_1_0_declassify(ctx, &rsk_scalar, sizeof(rsk_scalar));

    for (base = 0; base < n_tweaks; base += SP_BATCH_TWEAK_CHUNK) {
        size_t chunk = n_tweaks - base;
        size_t total;
        if (chunk > SP_BATCH_TWEAK_CHUNK) {
            chunk = SP_BATCH_TWEAK_CHUNK;
        }

        /* Phase A: shared-secret point ss_j = tweak * rsk for each tweak. The
         * tweaks are loaded into Jacobian form, then run through the interleaved
         * ECDH SP_ECDH_LANES at a time so the fixed-scalar wnaf schedule is
         * shared and the per-lane field ops pipeline. */
        for (t = 0; t < chunk; t++) {
            bwkspscan_v0_1_0_ge tweak_ge;
            if (!bwkspscan_v0_1_0_pubkey_load(ctx, &tweak_ge, &tweaks[base + t])) {
                bwkspscan_v0_1_0_scalar_clear(&rsk_scalar);
                return 0;
            }
            bwkspscan_v0_1_0_gej_set_ge(&ss_j[t], &tweak_ge);
        }
        for (t = 0; t < chunk; t += SP_ECDH_LANES) {
            size_t lanes = chunk - t;
            if (lanes > SP_ECDH_LANES) {
                lanes = SP_ECDH_LANES;
            }
            bwkspscan_v0_1_0_silentpayments_ecdh_lockstep(&ss_j[t], &ss_j[t], &rsk_scalar, lanes);
        }

        /* Phase B: one inversion brings the whole chunk to affine, then derive
         * t_0 per tweak. This batches the per-tweak fe_inv that the reference
         * does inside _create_shared_secret_vartime. */
        bwkspscan_v0_1_0_ge_set_all_gej_var(ss_ge, ss_j, chunk);
        for (t = 0; t < chunk; t++) {
            size_t len;
            bwkspscan_v0_1_0_declassify(ctx, &ss_ge[t], sizeof(ss_ge[t]));
            /* Serializing can only fail if ss is the point at infinity, which is
             * impossible for a valid tweak and scan key. */
            if (!bwkspscan_v0_1_0_eckey_pubkey_serialize(&ss_ge[t], shared_secret33, &len, 1)) {
                bwkspscan_v0_1_0_scalar_clear(&rsk_scalar);
                bwkspscan_v0_1_0_memclear(shared_secret33, sizeof(shared_secret33));
                return 0;
            }
            VERIFY_CHECK(len == 33);
            bwkspscan_v0_1_0_silentpayments_create_t_k(&t0_scalar[t], shared_secret33, 0);
        }

        /* Phase C: t_0 * G per tweak. */
        for (t = 0; t < chunk; t++) {
            bwkspscan_v0_1_0_ecmult_gen(&ctx->ecmult_gen_ctx, &t0_gej[t], &t0_scalar[t]);
        }

        /* Phases D + E: candidate = spend_point + t_0 * G for each (tweak, spend
         * point), brought to affine in bounded sub-chunks with one inversion per
         * sub-chunk. The flat candidate index is t * n_spend_points + s. */
        total = chunk * n_spend_points;
        {
            size_t done;
            for (done = 0; done < total; done += SP_CANDIDATE_BATCH) {
                size_t sub = total - done;
                size_t j;
                if (sub > SP_CANDIDATE_BATCH) {
                    sub = SP_CANDIDATE_BATCH;
                }
                for (j = 0; j < sub; j++) {
                    size_t flat = done + j;
                    bwkspscan_v0_1_0_ge spend_ge;
                    t = flat / n_spend_points;
                    s = flat % n_spend_points;
                    if (!bwkspscan_v0_1_0_pubkey_load(ctx, &spend_ge, &spend_points[s])) {
                        bwkspscan_v0_1_0_scalar_clear(&rsk_scalar);
                        bwkspscan_v0_1_0_memclear(shared_secret33, sizeof(shared_secret33));
                        return 0;
                    }
                    bwkspscan_v0_1_0_gej_add_ge_var(&candidate_gej[j], &t0_gej[t], &spend_ge, NULL);
                }
                bwkspscan_v0_1_0_ge_set_all_gej_var(candidate_ge, candidate_gej, sub);
                for (j = 0; j < sub; j++) {
                    size_t flat = done + j;
                    /* Adding the hashed tweak point to a valid spend point can only
                     * yield infinity with negligible probability. */
                    if (bwkspscan_v0_1_0_ge_is_infinity(&candidate_ge[j])) {
                        bwkspscan_v0_1_0_scalar_clear(&rsk_scalar);
                        bwkspscan_v0_1_0_memclear(shared_secret33, sizeof(shared_secret33));
                        return 0;
                    }
                    bwkspscan_v0_1_0_xonly_pubkey_save(&out_xonly[(base * n_spend_points) + flat], &candidate_ge[j]);
                }
            }
        }
    }

    /* Explicitly clear secrets. The shared secret and t_0 values are not strictly
     * "secret", but leaking them lets a third party link the transaction back to
     * the recipient address. */
    bwkspscan_v0_1_0_scalar_clear(&rsk_scalar);
    bwkspscan_v0_1_0_memclear(shared_secret33, sizeof(shared_secret33));
    for (t = 0; t < SP_BATCH_TWEAK_CHUNK; t++) {
        bwkspscan_v0_1_0_scalar_clear(&t0_scalar[t]);
        bwkspscan_v0_1_0_gej_clear(&t0_gej[t]);
    }
    *n_out = n_tweaks * n_spend_points;
    return 1;
}

#endif

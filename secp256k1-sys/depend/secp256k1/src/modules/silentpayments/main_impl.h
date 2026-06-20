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

/** Sort an array of silent payment recipients. This is used to group recipients by scan pubkey to
 *  ensure the correct values of k are used when creating multiple outputs for a recipient. */
static int rustsecp256k1_v0_10_0_silentpayments_recipient_sort_cmp(const void* pk1, const void* pk2, void *ctx) {
    return rustsecp256k1_v0_10_0_ec_pubkey_cmp((rustsecp256k1_v0_10_0_context *)ctx,
        &(*(const rustsecp256k1_v0_10_0_silentpayments_recipient **)pk1)->scan_pubkey,
        &(*(const rustsecp256k1_v0_10_0_silentpayments_recipient **)pk2)->scan_pubkey
    );
}

static void rustsecp256k1_v0_10_0_silentpayments_recipient_sort(const rustsecp256k1_v0_10_0_context* ctx, const rustsecp256k1_v0_10_0_silentpayments_recipient **recipients, size_t n_recipients) {

    /* Suppress wrong warning (fixed in MSVC 19.33) */
    #if defined(_MSC_VER) && (_MSC_VER < 1933)
    #pragma warning(push)
    #pragma warning(disable: 4090)
    #endif

    rustsecp256k1_v0_10_0_hsort(recipients, n_recipients, sizeof(*recipients), rustsecp256k1_v0_10_0_silentpayments_recipient_sort_cmp, (void *)ctx);

    #if defined(_MSC_VER) && (_MSC_VER < 1933)
    #pragma warning(pop)
    #endif
}

/** Set hash state to the BIP340 tagged hash midstate for "BIP0352/Inputs". */
static void rustsecp256k1_v0_10_0_silentpayments_sha256_init_inputs(rustsecp256k1_v0_10_0_sha256* hash) {
    rustsecp256k1_v0_10_0_sha256_initialize(hash);
    hash->s[0] = 0xd4143ffcul;
    hash->s[1] = 0x012ea4b5ul;
    hash->s[2] = 0x36e21c8ful;
    hash->s[3] = 0xf7ec7b54ul;
    hash->s[4] = 0x4dd4e2acul;
    hash->s[5] = 0x9bcaa0a4ul;
    hash->s[6] = 0xe244899bul;
    hash->s[7] = 0xcd06903eul;

    hash->bytes = 64;
}

static void rustsecp256k1_v0_10_0_silentpayments_calculate_input_hash(unsigned char *input_hash, const unsigned char *outpoint_smallest36, rustsecp256k1_v0_10_0_ge *pubkey_sum) {
    rustsecp256k1_v0_10_0_sha256 hash;
    unsigned char pubkey_sum_ser[33];
    size_t len;
    int ret;

    rustsecp256k1_v0_10_0_silentpayments_sha256_init_inputs(&hash);
    rustsecp256k1_v0_10_0_sha256_write(&hash, outpoint_smallest36, 36);
    ret = rustsecp256k1_v0_10_0_eckey_pubkey_serialize(pubkey_sum, pubkey_sum_ser, &len, 1);
    VERIFY_CHECK(ret && len == sizeof(pubkey_sum_ser));
    (void)ret;
    rustsecp256k1_v0_10_0_sha256_write(&hash, pubkey_sum_ser, sizeof(pubkey_sum_ser));
    rustsecp256k1_v0_10_0_sha256_finalize(&hash, input_hash);
}

static void rustsecp256k1_v0_10_0_silentpayments_create_shared_secret(const rustsecp256k1_v0_10_0_context *ctx, unsigned char *shared_secret33, const rustsecp256k1_v0_10_0_scalar *secret_component, const rustsecp256k1_v0_10_0_ge *public_component) {
    rustsecp256k1_v0_10_0_gej ss_j;
    rustsecp256k1_v0_10_0_ge ss;
    size_t len;
    int ret;

    /* Compute shared_secret = tweaked_secret_component * Public_component */
    rustsecp256k1_v0_10_0_ecmult_const(&ss_j, public_component, secret_component);
    rustsecp256k1_v0_10_0_ge_set_gej(&ss, &ss_j);
    rustsecp256k1_v0_10_0_declassify(ctx, &ss, sizeof(ss));
    /* This can only fail if the shared secret is the point at infinity, which should be
     * impossible at this point, considering we have already validated the public key and
     * the secret key being used
     */
    ret = rustsecp256k1_v0_10_0_eckey_pubkey_serialize(&ss, shared_secret33, &len, 1);
    VERIFY_CHECK(ret && len == 33);
    (void)ret;
    /* While not technically "secret" data, explicitly clear the shared secret since leaking this would allow an attacker
     * to identify the resulting transaction as a silent payments transaction and potentially link the transaction
     * back to the silent payment address
     */
    rustsecp256k1_v0_10_0_ge_clear(&ss);
    rustsecp256k1_v0_10_0_gej_clear(&ss_j);
}

static void rustsecp256k1_v0_10_0_silentpayments_create_shared_secret_vartime(const rustsecp256k1_v0_10_0_context *ctx, unsigned char *shared_secret33, const rustsecp256k1_v0_10_0_scalar *secret_component, const rustsecp256k1_v0_10_0_ge *public_component) {
    rustsecp256k1_v0_10_0_gej ss_j;
    rustsecp256k1_v0_10_0_gej a_gej;
    rustsecp256k1_v0_10_0_ge ss;
    rustsecp256k1_v0_10_0_scalar zero;
    size_t len;
    int ret;

    /* VARIABLE TIME, recipient scanning only, never signing */
    rustsecp256k1_v0_10_0_gej_set_ge(&a_gej, public_component);
    rustsecp256k1_v0_10_0_scalar_set_int(&zero, 0);
    rustsecp256k1_v0_10_0_ecmult(&ss_j, &a_gej, secret_component, &zero);
    rustsecp256k1_v0_10_0_ge_set_gej(&ss, &ss_j);
    rustsecp256k1_v0_10_0_declassify(ctx, &ss, sizeof(ss));
    /* This can only fail if the shared secret is the point at infinity, which should be
     * impossible at this point, considering we have already validated the public key and
     * the secret key being used
     */
    ret = rustsecp256k1_v0_10_0_eckey_pubkey_serialize(&ss, shared_secret33, &len, 1);
    VERIFY_CHECK(ret && len == 33);
    (void)ret;
    /* While not technically "secret" data, explicitly clear the shared secret since leaking this would allow an attacker
     * to identify the resulting transaction as a silent payments transaction and potentially link the transaction
     * back to the silent payment address
     */
    rustsecp256k1_v0_10_0_ge_clear(&ss);
    rustsecp256k1_v0_10_0_gej_clear(&ss_j);
}

/** Set hash state to the BIP340 tagged hash midstate for "BIP0352/SharedSecret". */
static void rustsecp256k1_v0_10_0_silentpayments_sha256_init_sharedsecret(rustsecp256k1_v0_10_0_sha256* hash) {
    rustsecp256k1_v0_10_0_sha256_initialize(hash);
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

static void rustsecp256k1_v0_10_0_silentpayments_create_t_k(rustsecp256k1_v0_10_0_scalar *t_k_scalar, const unsigned char *shared_secret33, uint32_t k) {
    rustsecp256k1_v0_10_0_sha256 hash;
    unsigned char hash_ser[32];
    unsigned char k_serialized[4];
    int overflow = 0;

    /* Compute t_k = hash(shared_secret || ser_32(k))  [sha256 with tag "BIP0352/SharedSecret"] */
    rustsecp256k1_v0_10_0_silentpayments_sha256_init_sharedsecret(&hash);
    rustsecp256k1_v0_10_0_sha256_write(&hash, shared_secret33, 33);
    rustsecp256k1_v0_10_0_write_be32(k_serialized, k);
    rustsecp256k1_v0_10_0_sha256_write(&hash, k_serialized, sizeof(k_serialized));
    rustsecp256k1_v0_10_0_sha256_finalize(&hash, hash_ser);
    rustsecp256k1_v0_10_0_scalar_set_b32(t_k_scalar, hash_ser, &overflow);
    VERIFY_CHECK(!overflow);
    VERIFY_CHECK(!rustsecp256k1_v0_10_0_scalar_is_zero(t_k_scalar));
    /* While not technically "secret" data, explicitly clear hash_ser since leaking this would allow an attacker
     * to identify the resulting transaction as a silent payments transaction and potentially link the transaction
     * back to the silent payment address
     */
    rustsecp256k1_v0_10_0_memclear(hash_ser, sizeof(hash_ser));
}

static int rustsecp256k1_v0_10_0_silentpayments_create_output_pubkey(const rustsecp256k1_v0_10_0_context *ctx, rustsecp256k1_v0_10_0_xonly_pubkey *P_output_xonly, const unsigned char *shared_secret33, const rustsecp256k1_v0_10_0_pubkey *recipient_labeled_spend_pubkey, uint32_t k) {
    rustsecp256k1_v0_10_0_ge P_output_ge;
    rustsecp256k1_v0_10_0_scalar t_k_scalar;
    int ret;

    /* Calculate and return P_output_xonly = B_spend + t_k * G
     * This will fail if B_spend is the point at infinity or if
     * B_spend + t_k*G is the point at infinity.
     */
    rustsecp256k1_v0_10_0_silentpayments_create_t_k(&t_k_scalar, shared_secret33, k);
    if (!rustsecp256k1_v0_10_0_pubkey_load(ctx, &P_output_ge, recipient_labeled_spend_pubkey)) {
        rustsecp256k1_v0_10_0_scalar_clear(&t_k_scalar);
        return 0;
    }
    ret = rustsecp256k1_v0_10_0_eckey_pubkey_tweak_add(&P_output_ge, &t_k_scalar);
    /* tweak add only fails if t_k_scalar is equal to the dlog of P_output_ge, but t_k_scalar is the output of a collision resistant hash function. */
    /* TODO: consider declassify ret */
    /* TODO: but we don't want to imply this can never happen */
    VERIFY_CHECK(ret);
#ifndef VERIFY
    (void) ret;
#endif
    rustsecp256k1_v0_10_0_xonly_pubkey_save(P_output_xonly, &P_output_ge);

    /* While not technically "secret" data, explicitly clear t_k since leaking this would allow an attacker
     * to identify the resulting transaction as a silent payments transaction and potentially link the transaction
     * back to the silent payment address
     */
    rustsecp256k1_v0_10_0_scalar_clear(&t_k_scalar);
    return 1;
}

int rustsecp256k1_v0_10_0_silentpayments_sender_create_outputs(
    const rustsecp256k1_v0_10_0_context *ctx,
    rustsecp256k1_v0_10_0_xonly_pubkey **generated_outputs,
    const rustsecp256k1_v0_10_0_silentpayments_recipient **recipients,
    size_t n_recipients,
    const unsigned char *outpoint_smallest36,
    const rustsecp256k1_v0_10_0_keypair * const *taproot_seckeys,
    size_t n_taproot_seckeys,
    const unsigned char * const *plain_seckeys,
    size_t n_plain_seckeys
) {
    size_t i, k;
    rustsecp256k1_v0_10_0_scalar a_sum_scalar, addend, input_hash_scalar;
    rustsecp256k1_v0_10_0_ge A_sum_ge;
    rustsecp256k1_v0_10_0_gej A_sum_gej;
    unsigned char input_hash[32];
    unsigned char shared_secret[33];
    rustsecp256k1_v0_10_0_silentpayments_recipient last_recipient;
    int overflow = 0;
    int ret;

    /* Sanity check inputs. */
    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(rustsecp256k1_v0_10_0_ecmult_gen_context_is_built(&ctx->ecmult_gen_ctx));
    ARG_CHECK(generated_outputs != NULL);
    ARG_CHECK(recipients != NULL);
    ARG_CHECK(n_recipients > 0);
    ARG_CHECK((plain_seckeys != NULL) || (taproot_seckeys != NULL));
    if (taproot_seckeys != NULL) {
        ARG_CHECK(n_taproot_seckeys > 0);
    } else {
        ARG_CHECK(n_taproot_seckeys == 0);
    }
    if (plain_seckeys != NULL) {
        ARG_CHECK(n_plain_seckeys > 0);
    } else {
        ARG_CHECK(n_plain_seckeys == 0);
    }
    ARG_CHECK(outpoint_smallest36 != NULL);
    /* ensure the index field is set correctly */
    for (i = 0; i < n_recipients; i++) {
        ARG_CHECK(recipients[i]->index == i);
    }

    /* Compute input private keys sum: a_sum = a_1 + a_2 + ... + a_n */
    a_sum_scalar = rustsecp256k1_v0_10_0_scalar_zero;
    for (i = 0; i < n_plain_seckeys; i++) {
        ret = rustsecp256k1_v0_10_0_scalar_set_b32_seckey(&addend, plain_seckeys[i]);
        rustsecp256k1_v0_10_0_declassify(ctx, &ret, sizeof(ret));
        if (!ret) {
            rustsecp256k1_v0_10_0_scalar_clear(&addend);
            rustsecp256k1_v0_10_0_scalar_clear(&a_sum_scalar);
            return 0;
        }
        rustsecp256k1_v0_10_0_scalar_add(&a_sum_scalar, &a_sum_scalar, &addend);
    }
    /* private keys used for taproot outputs have to be negated if they resulted in an odd point */
    for (i = 0; i < n_taproot_seckeys; i++) {
        rustsecp256k1_v0_10_0_ge addend_point;
        ret = rustsecp256k1_v0_10_0_keypair_load(ctx, &addend, &addend_point, taproot_seckeys[i]);
        rustsecp256k1_v0_10_0_declassify(ctx, &ret, sizeof(ret));
        if (!ret) {
            rustsecp256k1_v0_10_0_scalar_clear(&addend);
            rustsecp256k1_v0_10_0_scalar_clear(&a_sum_scalar);
            return 0;
        }
        rustsecp256k1_v0_10_0_declassify(ctx, &ret, sizeof(ret));
        if (rustsecp256k1_v0_10_0_fe_is_odd(&addend_point.y)) {
            rustsecp256k1_v0_10_0_scalar_negate(&addend, &addend);
        }
        rustsecp256k1_v0_10_0_scalar_add(&a_sum_scalar, &a_sum_scalar, &addend);
    }
    /* If there are any failures in loading/summing up the secret keys, fail early */
    ret = rustsecp256k1_v0_10_0_scalar_is_zero(&a_sum_scalar);
    rustsecp256k1_v0_10_0_declassify(ctx, &ret, sizeof(ret));
    if (ret) {
        rustsecp256k1_v0_10_0_scalar_clear(&addend);
        rustsecp256k1_v0_10_0_scalar_clear(&a_sum_scalar);
        return 0;
    } else {
        /* Clear the addend variable as this is no longer needed at this point
         * and contains secret data. This saves from needing to remember to clear
         * this variable from multiple places below */
        rustsecp256k1_v0_10_0_scalar_clear(&addend);
    }
    /* Compute input_hash = hash(outpoint_L || (a_sum * G)) */
    rustsecp256k1_v0_10_0_ecmult_gen(&ctx->ecmult_gen_ctx, &A_sum_gej, &a_sum_scalar);
    rustsecp256k1_v0_10_0_ge_set_gej(&A_sum_ge, &A_sum_gej);
    /* TODO: comment */
    rustsecp256k1_v0_10_0_declassify(ctx, &A_sum_ge, sizeof(A_sum_ge));

    /* Calculate the input hash and tweak a_sum, i.e., a_sum_tweaked = a_sum * input_hash
     * This should fail if input hash is greater than the curve order, but this is stastically improbable so
     * we only do a verify_check here.
     */
    rustsecp256k1_v0_10_0_silentpayments_calculate_input_hash(input_hash, outpoint_smallest36, &A_sum_ge);
    rustsecp256k1_v0_10_0_scalar_set_b32(&input_hash_scalar, input_hash, &overflow);
    VERIFY_CHECK(!overflow);
    rustsecp256k1_v0_10_0_scalar_mul(&a_sum_scalar, &a_sum_scalar, &input_hash_scalar);
    /* _recipient_sort sorts the array of recipients in place by their scan public keys (lexicographically).
     * This ensures that all recipients with the same scan public key are grouped together, as specified in BIP0352.
     *
     * More specifically, this ensures `k` is incremented from 0 to the number of requested outputs for each recipient group,
     * where a recipient group is all addresses with the same scan public key.
     */
    rustsecp256k1_v0_10_0_silentpayments_recipient_sort(ctx, recipients, n_recipients);
    last_recipient = *recipients[0];
    k = 0;
    for (i = 0; i < n_recipients; i++) {
        if ((i == 0) || (rustsecp256k1_v0_10_0_ec_pubkey_cmp(ctx, &last_recipient.scan_pubkey, &recipients[i]->scan_pubkey) != 0)) {
            /* If we are on a different scan pubkey, its time to recreate the shared secret and reset k to 0.
             * It's very unlikely the scan public key is invalid by this point, since this means the caller would
             * have created the _silentpayments_recipient object incorrectly, but just to be sure we still check that
             * the public key is valid.
             */
            rustsecp256k1_v0_10_0_ge pk;
            if (!rustsecp256k1_v0_10_0_pubkey_load(ctx, &pk, &recipients[i]->scan_pubkey)) {
                rustsecp256k1_v0_10_0_scalar_clear(&a_sum_scalar);
                return 0;
            }
            rustsecp256k1_v0_10_0_silentpayments_create_shared_secret(ctx, shared_secret, &a_sum_scalar, &pk);
            k = 0;
        }
        if (!rustsecp256k1_v0_10_0_silentpayments_create_output_pubkey(ctx, generated_outputs[recipients[i]->index], shared_secret, &recipients[i]->labeled_spend_pubkey, k)) {
            rustsecp256k1_v0_10_0_scalar_clear(&a_sum_scalar);
            return 0;
        }
        k++;
        last_recipient = *recipients[i];
    }
    /* Explicitly clear variables containing secret data */
    rustsecp256k1_v0_10_0_scalar_clear(&a_sum_scalar);

    /* While technically not "secret data," explicitly clear the shared secret since leaking this
     * could result in a third party being able to identify the transaction as a silent payments transaction
     * and potentially link the transaction back to a silent payment address
     */
    rustsecp256k1_v0_10_0_memclear(&shared_secret, sizeof(shared_secret));
    return 1;
}

/** Set hash state to the BIP340 tagged hash midstate for "BIP0352/Label". */
static void rustsecp256k1_v0_10_0_silentpayments_sha256_init_label(rustsecp256k1_v0_10_0_sha256* hash) {
    rustsecp256k1_v0_10_0_sha256_initialize(hash);
    hash->s[0] = 0x26b95d63ul;
    hash->s[1] = 0x8bf1b740ul;
    hash->s[2] = 0x10a5986ful;
    hash->s[3] = 0x06a387a5ul;
    hash->s[4] = 0x2d1c1c30ul;
    hash->s[5] = 0xd035951aul;
    hash->s[6] = 0x2d7f0f96ul;
    hash->s[7] = 0x29e3e0dbul;

    hash->bytes = 64;
}

int rustsecp256k1_v0_10_0_silentpayments_recipient_create_label(const rustsecp256k1_v0_10_0_context *ctx, rustsecp256k1_v0_10_0_pubkey *label, unsigned char *label_tweak32, const unsigned char *recipient_scan_key32, const uint32_t m) {
    rustsecp256k1_v0_10_0_sha256 hash;
    unsigned char m_serialized[4];

    /* Sanity check inputs. */
    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(label != NULL);
    ARG_CHECK(label_tweak32 != NULL);
    ARG_CHECK(recipient_scan_key32 != NULL);

    /* Compute label_tweak = hash(ser_256(b_scan) || ser_32(m))  [sha256 with tag "BIP0352/Label"] */
    rustsecp256k1_v0_10_0_silentpayments_sha256_init_label(&hash);
    rustsecp256k1_v0_10_0_sha256_write(&hash, recipient_scan_key32, 32);
    rustsecp256k1_v0_10_0_write_be32(m_serialized, m);
    rustsecp256k1_v0_10_0_sha256_write(&hash, m_serialized, sizeof(m_serialized));
    rustsecp256k1_v0_10_0_sha256_finalize(&hash, label_tweak32);

    /* Compute label = label_tweak * G */
    return rustsecp256k1_v0_10_0_ec_pubkey_create(ctx, label, label_tweak32);
}

int rustsecp256k1_v0_10_0_silentpayments_recipient_create_labeled_spend_pubkey(const rustsecp256k1_v0_10_0_context *ctx, rustsecp256k1_v0_10_0_pubkey *labeled_spend_pubkey, const rustsecp256k1_v0_10_0_pubkey *recipient_spend_pubkey, const rustsecp256k1_v0_10_0_pubkey *label) {
    rustsecp256k1_v0_10_0_ge B_m, label_addend;
    rustsecp256k1_v0_10_0_gej result_gej;
    rustsecp256k1_v0_10_0_ge result_ge;
    int ret;

    /* Sanity check inputs. */
    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(labeled_spend_pubkey != NULL);
    ARG_CHECK(recipient_spend_pubkey != NULL);
    ARG_CHECK(label != NULL);

    /* Calculate B_m = B_spend + label
     * If either the label or spend public key is an invalid public key,
     * return early
     */
    ret = rustsecp256k1_v0_10_0_pubkey_load(ctx, &B_m, recipient_spend_pubkey);
    ret &= rustsecp256k1_v0_10_0_pubkey_load(ctx, &label_addend, label);
    if (!ret) {
        return ret;
    }
    rustsecp256k1_v0_10_0_gej_set_ge(&result_gej, &B_m);
    rustsecp256k1_v0_10_0_gej_add_ge_var(&result_gej, &result_gej, &label_addend, NULL);
    if (rustsecp256k1_v0_10_0_gej_is_infinity(&result_gej)) {
        return 0;
    }

    rustsecp256k1_v0_10_0_ge_set_gej(&result_ge, &result_gej);
    rustsecp256k1_v0_10_0_pubkey_save(labeled_spend_pubkey, &result_ge);

    return 1;
}

/** A explanation of the public_data object and its usage:
 *
 *  The public_data object contains:
 *
 *  [magic: 4 bytes][boolean: 1 byte][A_sum: 64 bytes][input_hash: 32 bytes]
 *
 *  The magic bytes are checked by functions using the public_data object to
 *  check that the public data object was initialized correctly.
 *
 *  The boolean (combined) indicates whether or not the A_sum point and the
 *  input_hash scalar have already been combined or are both included. The reason
 *  for keeping input_hash and A_sum separate is so that an elliptic curve multiplication
 *  can be avoided when creating the shared secret, i.e., (b_scan * input_hash) * A_sum.
 *
 *  But when storing the public data object, either to send to light clients or for
 *  wallet rescans, we can save 32-bytes by combining the input_hash and A_sum and saving
 *  the resulting point serialized as a compressed public key, i.e., input_hash * A_sum.
 *
 *  For the each function:
 *
 *  - `_recipient_public_data_create` always creates a public_data object with combined = false
 *  - `_recipient_public_data_serialize` only accepts a public_data object with combined = false
 *    and then performs an EC mult before serializing the resulting public key as a compressed
 *    public key
 *  - `_recpient_public_data_parse` assumes the input represents a previously serialized
 *    public_data object and always deserializes into a public_data object with combined = true
 *    (and the input_hash portion zeroed out).
 */

int rustsecp256k1_v0_10_0_silentpayments_recipient_public_data_create(
    const rustsecp256k1_v0_10_0_context *ctx,
    rustsecp256k1_v0_10_0_silentpayments_recipient_public_data *public_data,
    const unsigned char *outpoint_smallest36,
    const rustsecp256k1_v0_10_0_xonly_pubkey * const *xonly_pubkeys,
    size_t n_xonly_pubkeys,
    const rustsecp256k1_v0_10_0_pubkey * const *plain_pubkeys,
    size_t n_plain_pubkeys
) {
    size_t i;
    size_t pubkeylen = 64;
    rustsecp256k1_v0_10_0_ge A_sum_ge, addend;
    rustsecp256k1_v0_10_0_gej A_sum_gej;
    rustsecp256k1_v0_10_0_scalar input_hash_scalar;
    unsigned char input_hash_local[32];
    int overflow = 0;
    int ret = 1;

    /* Sanity check inputs */
    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(public_data != NULL);
    ARG_CHECK(outpoint_smallest36 != NULL);
    ARG_CHECK((plain_pubkeys != NULL) || (xonly_pubkeys != NULL));
    if (xonly_pubkeys != NULL) {
        ARG_CHECK(n_xonly_pubkeys > 0);
    } else {
        ARG_CHECK(n_xonly_pubkeys == 0);
    }
    if (plain_pubkeys != NULL) {
        ARG_CHECK(n_plain_pubkeys > 0);
    } else {
        ARG_CHECK(n_plain_pubkeys == 0);
    }
    rustsecp256k1_v0_10_0_memclear(input_hash_local, 32);

    /* Compute input public keys sum: A_sum = A_1 + A_2 + ... + A_n */
    rustsecp256k1_v0_10_0_gej_set_infinity(&A_sum_gej);
    for (i = 0; i < n_plain_pubkeys; i++) {
        ret &= rustsecp256k1_v0_10_0_pubkey_load(ctx, &addend, plain_pubkeys[i]);
        rustsecp256k1_v0_10_0_gej_add_ge_var(&A_sum_gej, &A_sum_gej, &addend, NULL);
    }
    for (i = 0; i < n_xonly_pubkeys; i++) {
        ret &= rustsecp256k1_v0_10_0_xonly_pubkey_load(ctx, &addend, xonly_pubkeys[i]);
        rustsecp256k1_v0_10_0_gej_add_ge_var(&A_sum_gej, &A_sum_gej, &addend, NULL);
    }
    /* Since an attacker can maliciously craft transactions where the public keys sum to zero, fail early here
     * to avoid making the caller do extra work, e.g., when building an index or scanning many malicious transactions
     *
     * This will also fail if any of the provided input public keys are malformed.
     */
    if (!ret || rustsecp256k1_v0_10_0_gej_is_infinity(&A_sum_gej)) {
        return 0;
    }
    /* Compute input_hash = hash(outpoint_L || A_sum) */
    rustsecp256k1_v0_10_0_ge_set_gej(&A_sum_ge, &A_sum_gej);
    rustsecp256k1_v0_10_0_silentpayments_calculate_input_hash(input_hash_local, outpoint_smallest36, &A_sum_ge);
    /* Convert input_hash to a scalar to ensure the value is less than the curve order.
     *
     * This can only fail if the output of the hash function is greater than the curve order, which
     * is statistically improbable.
     *
     * TODO: BIP0352 is currently underspecified with respect to input_hash, update to specify how to
     * handle the conversion of input_hash to a scalar.*/
    rustsecp256k1_v0_10_0_scalar_set_b32(&input_hash_scalar, input_hash_local, &overflow);
    VERIFY_CHECK(!overflow);
    /* serialize the public_data struct */
    memcpy(&public_data->data[0], rustsecp256k1_v0_10_0_silentpayments_public_data_magic, 4);
    public_data->data[4] = 0;
    rustsecp256k1_v0_10_0_ge_to_bytes(&public_data->data[5], &A_sum_ge);
    memcpy(&public_data->data[5 + pubkeylen], input_hash_local, 32);
    return 1;
}

static int rustsecp256k1_v0_10_0_silentpayments_recipient_public_data_load_pubkey(const rustsecp256k1_v0_10_0_context* ctx, rustsecp256k1_v0_10_0_ge *ge, const rustsecp256k1_v0_10_0_silentpayments_recipient_public_data *public_data) {
    ARG_CHECK(rustsecp256k1_v0_10_0_memcmp_var(&public_data->data[0], rustsecp256k1_v0_10_0_silentpayments_public_data_magic, 4) == 0);
    rustsecp256k1_v0_10_0_ge_from_bytes(ge, &public_data->data[5]);
    return 1;
}

static int rustsecp256k1_v0_10_0_silentpayments_recipient_public_data_load_input_hash(const rustsecp256k1_v0_10_0_context* ctx, rustsecp256k1_v0_10_0_scalar *input_hash_scalar, const rustsecp256k1_v0_10_0_silentpayments_recipient_public_data *public_data) {
    ARG_CHECK(rustsecp256k1_v0_10_0_memcmp_var(&public_data->data[0], rustsecp256k1_v0_10_0_silentpayments_public_data_magic, 4) == 0);
    rustsecp256k1_v0_10_0_scalar_set_b32(input_hash_scalar, &public_data->data[5 + 64], NULL);
    return 1;
}

int rustsecp256k1_v0_10_0_silentpayments_recipient_public_data_serialize(const rustsecp256k1_v0_10_0_context *ctx, unsigned char *output33, const rustsecp256k1_v0_10_0_silentpayments_recipient_public_data *public_data) {
    rustsecp256k1_v0_10_0_ge ge;
    rustsecp256k1_v0_10_0_scalar input_hash_scalar;
    size_t pubkeylen = 33;

    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(output33 != NULL);
    ARG_CHECK(public_data != NULL);
    ARG_CHECK(rustsecp256k1_v0_10_0_memcmp_var(&public_data->data[0], rustsecp256k1_v0_10_0_silentpayments_public_data_magic, 4) == 0);
    /* Only allow public_data to be serialized if it has the hash and the summed public key
     * This helps protect against accidentally serializing just the summed public key A
     */
    ARG_CHECK(public_data->data[4] == 0);
    /* These functions should never fail at this point considering:
     *   - `_public_data_load` functions can only fail if the public data object was created incorrectly
     *     and we already check for this above
     *   - `_tweak_mul` can only fail if input_hash_scalar is zero, but assuming the public_data object
     *     was created correctly, this is impossible because input_hash_scalar is the output of a hash function
     *   - `_eckey_pubkey_serialize` can only fail if the point we are trying to serialize is the point at infinity
     *
     *   Note: we don't verify that the input hash is less than the curve order since this is verified when the
     *   public data object is created.
     */
    rustsecp256k1_v0_10_0_silentpayments_recipient_public_data_load_pubkey(ctx, &ge, public_data);
    rustsecp256k1_v0_10_0_silentpayments_recipient_public_data_load_input_hash(ctx, &input_hash_scalar, public_data);
    rustsecp256k1_v0_10_0_eckey_pubkey_tweak_mul(&ge, &input_hash_scalar);
    rustsecp256k1_v0_10_0_eckey_pubkey_serialize(&ge, output33, &pubkeylen, 1);
    VERIFY_CHECK(pubkeylen == 33);
    return 1;
}

int rustsecp256k1_v0_10_0_silentpayments_recipient_public_data_parse(const rustsecp256k1_v0_10_0_context *ctx, rustsecp256k1_v0_10_0_silentpayments_recipient_public_data *public_data, const unsigned char *input33) {
    size_t inputlen = 33;
    rustsecp256k1_v0_10_0_ge pk;

    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(public_data != NULL);
    ARG_CHECK(input33 != NULL);
    /* Since an attacker can send us malicious data that looks like a serialized public key but is not, fail early */
    if (!rustsecp256k1_v0_10_0_eckey_pubkey_parse(&pk, input33, inputlen)) {
        return 0;
    }
    /* A serialized public data will always have have the input_hash multiplied in, so we set combined = true.
     * Additionally, we zero out the 32 bytes where the input_hash would be
     */
    memcpy(&public_data->data[0], rustsecp256k1_v0_10_0_silentpayments_public_data_magic, 4);
    public_data->data[4] = 1;
    rustsecp256k1_v0_10_0_ge_to_bytes(&public_data->data[5], &pk);
    memset(&public_data->data[5 + 64], 0, 32);
    return 1;
}

int rustsecp256k1_v0_10_0_silentpayments_recipient_scan_outputs(
    const rustsecp256k1_v0_10_0_context *ctx,
    rustsecp256k1_v0_10_0_silentpayments_found_output **found_outputs, size_t *n_found_outputs,
    const rustsecp256k1_v0_10_0_xonly_pubkey * const *tx_outputs, size_t n_tx_outputs,
    const unsigned char *recipient_scan_key32,
    const rustsecp256k1_v0_10_0_silentpayments_recipient_public_data *public_data,
    const rustsecp256k1_v0_10_0_pubkey *recipient_spend_pubkey,
    const rustsecp256k1_v0_10_0_silentpayments_label_lookup label_lookup,
    const void *label_context
) {
    rustsecp256k1_v0_10_0_scalar t_k_scalar, rsk_scalar;
    rustsecp256k1_v0_10_0_ge label_ge, recipient_spend_pubkey_ge, A_sum_ge;
    rustsecp256k1_v0_10_0_xonly_pubkey P_output_xonly;
    unsigned char shared_secret[33];
    const unsigned char *label_tweak = NULL;
    size_t i, k, n_found, found_idx;
    int found, combined;
    int ret = 1;

    /* Sanity check inputs */
    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(found_outputs != NULL);
    ARG_CHECK(n_found_outputs != NULL);
    ARG_CHECK(tx_outputs != NULL);
    ARG_CHECK(n_tx_outputs > 0);
    ARG_CHECK(recipient_scan_key32 != NULL);
    ARG_CHECK(public_data != NULL);
    ARG_CHECK(recipient_spend_pubkey != NULL);
    /* Passing a context without a lookup function is non-sensical */
    if (label_context != NULL) {
        ARG_CHECK(label_lookup != NULL);
    }
    /* Recall: a scan key isnt really "secret" data in that leaking the scan key will only leak privacy.
     *
     * However, if there is something wrong with the recipient scan key, recipient spend pubkey, or the public data,
     * we fail early and make sure to clear the scan key from memory. */
    ret = rustsecp256k1_v0_10_0_scalar_set_b32_seckey(&rsk_scalar, recipient_scan_key32);
    rustsecp256k1_v0_10_0_declassify(ctx, &ret, sizeof(ret));
    if (!ret) {
        rustsecp256k1_v0_10_0_scalar_clear(&rsk_scalar);
        return 0;
    }
    ret = rustsecp256k1_v0_10_0_silentpayments_recipient_public_data_load_pubkey(ctx, &A_sum_ge, public_data);
    combined = (int)public_data->data[4];
    if (!combined) {
        rustsecp256k1_v0_10_0_scalar input_hash_scalar;
        ret &= rustsecp256k1_v0_10_0_silentpayments_recipient_public_data_load_input_hash(ctx, &input_hash_scalar, public_data);
        rustsecp256k1_v0_10_0_scalar_mul(&rsk_scalar, &rsk_scalar, &input_hash_scalar);
    }
    ret &= rustsecp256k1_v0_10_0_pubkey_load(ctx, &recipient_spend_pubkey_ge, recipient_spend_pubkey);
    if (!ret) {
        rustsecp256k1_v0_10_0_scalar_clear(&rsk_scalar);
        return 0;
    }
    rustsecp256k1_v0_10_0_declassify(ctx, &rsk_scalar, sizeof(rsk_scalar));
    rustsecp256k1_v0_10_0_silentpayments_create_shared_secret_vartime(ctx, shared_secret, &rsk_scalar, &A_sum_ge);

    found_idx = 0;
    n_found = 0;
    k = 0;
    while (1) {
        rustsecp256k1_v0_10_0_ge P_output_ge = recipient_spend_pubkey_ge;
        /* Calculate t_k = hash(shared_secret || ser_32(k)) */
        rustsecp256k1_v0_10_0_silentpayments_create_t_k(&t_k_scalar, shared_secret, k);

        /* Calculate P_output = B_spend + t_k * G
         * This can fail if t_k is the negation of B_spend, but this is statistically
         * improbable as t_k is the output of a hash function. */
        ret = rustsecp256k1_v0_10_0_eckey_pubkey_tweak_add(&P_output_ge, &t_k_scalar);
        VERIFY_CHECK(ret);
        found = 0;
        rustsecp256k1_v0_10_0_xonly_pubkey_save(&P_output_xonly, &P_output_ge);
        for (i = 0; i < n_tx_outputs; i++) {
            if (rustsecp256k1_v0_10_0_xonly_pubkey_cmp(ctx, &P_output_xonly, tx_outputs[i]) == 0) {
                label_tweak = NULL;
                found = 1;
                found_idx = i;
                break;
            }

            /* If not found, proceed to check for labels (if the labels cache is present) */
            if (label_lookup != NULL) {
                rustsecp256k1_v0_10_0_ge P_output_negated_ge, tx_output_ge;
                rustsecp256k1_v0_10_0_gej tx_output_gej, label_gej;
                unsigned char label33[33];
                size_t len;

                rustsecp256k1_v0_10_0_xonly_pubkey_load(ctx, &tx_output_ge, tx_outputs[i]);
                rustsecp256k1_v0_10_0_gej_set_ge(&tx_output_gej, &tx_output_ge);
                rustsecp256k1_v0_10_0_ge_neg(&P_output_negated_ge, &P_output_ge);
                /* Negate the generated output and calculate first scan label candidate:
                 * label1 = tx_output - P_output */
                rustsecp256k1_v0_10_0_gej_add_ge_var(&label_gej, &tx_output_gej, &P_output_negated_ge, NULL);
                rustsecp256k1_v0_10_0_ge_set_gej(&label_ge, &label_gej);
                ret = rustsecp256k1_v0_10_0_eckey_pubkey_serialize(&label_ge, label33, &len, 1);
                /* serialize must succeed because the point was just loaded */
                VERIFY_CHECK(ret && len == 33);
                label_tweak = label_lookup(label33, label_context);
                if (label_tweak != NULL) {
                    found = 1;
                    found_idx = i;
                    break;
                }

                rustsecp256k1_v0_10_0_gej_neg(&label_gej, &tx_output_gej);
                /* If not found, negate the tx_output and calculate second scan label candidate:
                 * label2 = -tx_output - P_output */
                rustsecp256k1_v0_10_0_gej_add_ge_var(&label_gej, &label_gej, &P_output_negated_ge, NULL);
                rustsecp256k1_v0_10_0_ge_set_gej(&label_ge, &label_gej);
                ret = rustsecp256k1_v0_10_0_eckey_pubkey_serialize(&label_ge, label33, &len, 1);
                /* serialize must succeed because the point was just loaded */
                VERIFY_CHECK(ret && len == 33);
                label_tweak = label_lookup(label33, label_context);
                if (label_tweak != NULL) {
                    found = 1;
                    found_idx = i;
                    break;
                }
            }
        }
        if (found) {
            found_outputs[n_found]->output = *tx_outputs[found_idx];
            rustsecp256k1_v0_10_0_scalar_get_b32(found_outputs[n_found]->tweak, &t_k_scalar);
            if (label_tweak != NULL) {
                found_outputs[n_found]->found_with_label = 1;
                /* This is extremely unlikely to fail in that it can only really fail if label_tweak
                 * is the negation of the shared secret tweak. But since both tweak and label_tweak are
                 * created by hashing data, practically speaking this would only happen if an attacker
                 * tricked us into using a particular label_tweak (deviating from the protocol).
                 */
                ret = rustsecp256k1_v0_10_0_ec_seckey_tweak_add(ctx, found_outputs[n_found]->tweak, label_tweak);
                VERIFY_CHECK(ret);
                rustsecp256k1_v0_10_0_pubkey_save(&found_outputs[n_found]->label, &label_ge);
            } else {
                found_outputs[n_found]->found_with_label = 0;
                /* Set the label public key with an invalid public key value */
                rustsecp256k1_v0_10_0_memclear(&found_outputs[n_found]->label, sizeof(rustsecp256k1_v0_10_0_pubkey));
            }
            /* Set everything for the next round of scanning */
            label_tweak = NULL;
            n_found++;
            k++;
        } else {
            break;
        }
    }
    *n_found_outputs = n_found;
    /* Explicitly clear secrets. Recall that the scan key is not quite "secret" in that leaking the scan key
     * results in a loss of privacy, not a loss of funds
     */
    rustsecp256k1_v0_10_0_scalar_clear(&rsk_scalar);
    /* Explicitly clear the shared secret. While this isn't technically "secret data," any third party
     * with access to the shared secret could potentially identify and link the transaction back to the
     * recipient address
     */
    rustsecp256k1_v0_10_0_scalar_clear(&t_k_scalar);
    rustsecp256k1_v0_10_0_memclear(shared_secret, sizeof(shared_secret));
    return ret;
}

int rustsecp256k1_v0_10_0_silentpayments_recipient_create_shared_secret(const rustsecp256k1_v0_10_0_context *ctx, unsigned char *shared_secret33, const unsigned char *recipient_scan_key32, const rustsecp256k1_v0_10_0_silentpayments_recipient_public_data *public_data) {
    rustsecp256k1_v0_10_0_scalar rsk;
    rustsecp256k1_v0_10_0_ge A_tweaked_ge;
    int ret = 1;
    /* Sanity check inputs */
    ARG_CHECK(shared_secret33 != NULL);
    ARG_CHECK(recipient_scan_key32 != NULL);
    ARG_CHECK(public_data != NULL);
    ARG_CHECK(public_data->data[4] == 1);
    /* TODO: do we need a _cmov operation here to avoid leaking information about the scan key?
     * Recall: a scan key is not really "secret" data, its functionally the same as an xpub
     */
    ret &= rustsecp256k1_v0_10_0_scalar_set_b32_seckey(&rsk, recipient_scan_key32);
    ret &= rustsecp256k1_v0_10_0_silentpayments_recipient_public_data_load_pubkey(ctx, &A_tweaked_ge, public_data);
    /* If there are any issues with the recipient scan key or public data, return early */
    if (!ret) {
        return 0;
    }
    rustsecp256k1_v0_10_0_silentpayments_create_shared_secret_vartime(ctx, shared_secret33, &rsk, &A_tweaked_ge);

    /* Explicitly clear secrets */
    rustsecp256k1_v0_10_0_scalar_clear(&rsk);
    return ret;
}

static int rustsecp256k1_v0_10_0_silentpayments_append_lightclient_candidate(
    const rustsecp256k1_v0_10_0_context *ctx,
    rustsecp256k1_v0_10_0_xonly_pubkey *candidate_xonly,
    size_t candidate_capacity,
    size_t *n_candidates,
    const unsigned char *shared_secret33,
    const rustsecp256k1_v0_10_0_pubkey *recipient_spend_pubkey,
    uint32_t k
) {
    if (*n_candidates >= candidate_capacity) {
        return 0;
    }
    if (!rustsecp256k1_v0_10_0_silentpayments_create_output_pubkey(ctx, &candidate_xonly[*n_candidates], shared_secret33, recipient_spend_pubkey, k)) {
        return 0;
    }
    *n_candidates += 1;
    return 1;
}

int rustsecp256k1_v0_10_0_silentpayments_recipient_scan_lightclient(
    const rustsecp256k1_v0_10_0_context *ctx,
    rustsecp256k1_v0_10_0_xonly_pubkey *candidate_xonly,
    size_t *n_candidates,
    const rustsecp256k1_v0_10_0_pubkey *combined_tweak,
    const unsigned char *recipient_scan_key32,
    const rustsecp256k1_v0_10_0_pubkey *recipient_spend_pubkey,
    const rustsecp256k1_v0_10_0_silentpayments_label_lookup label_lookup,
    const void *label_context,
    const uint32_t *label_integers,
    size_t n_label_integers
)
{
    unsigned char shared_secret33[33];
    rustsecp256k1_v0_10_0_ge combined_tweak_ge;
    rustsecp256k1_v0_10_0_pubkey label_pubkey;
    rustsecp256k1_v0_10_0_pubkey labeled_spend_pubkey;
    unsigned char label_tweak32[32];
    unsigned char label_ser[33];
    size_t label_ser_len;
    size_t candidate_capacity;
    size_t n_out;
    size_t i;
    uint32_t m;
    int ret;

    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(candidate_xonly != NULL);
    ARG_CHECK(n_candidates != NULL);
    ARG_CHECK(combined_tweak != NULL);
    ARG_CHECK(recipient_scan_key32 != NULL);
    ARG_CHECK(recipient_spend_pubkey != NULL);
    if (label_context != NULL) {
        ARG_CHECK(label_lookup != NULL);
    }
    if (n_label_integers > 0) {
        ARG_CHECK(label_lookup != NULL);
        ARG_CHECK(label_integers != NULL);
    }

    candidate_capacity = *n_candidates;
    n_out = 0;

    ret = rustsecp256k1_v0_10_0_pubkey_load(ctx, &combined_tweak_ge, combined_tweak);
    if (!ret) {
        return 0;
    }
    /* Recompute the shared secret with the scan key in scalar form. */
    {
        rustsecp256k1_v0_10_0_scalar rsk_scalar;
        ret = rustsecp256k1_v0_10_0_scalar_set_b32_seckey(&rsk_scalar, recipient_scan_key32);
        if (!ret) {
            rustsecp256k1_v0_10_0_memclear(shared_secret33, sizeof(shared_secret33));
            return 0;
        }
        rustsecp256k1_v0_10_0_declassify(ctx, &rsk_scalar, sizeof(rsk_scalar));
        rustsecp256k1_v0_10_0_silentpayments_create_shared_secret_vartime(ctx, shared_secret33, &rsk_scalar, &combined_tweak_ge);
        rustsecp256k1_v0_10_0_scalar_clear(&rsk_scalar);
    }

    ret = rustsecp256k1_v0_10_0_silentpayments_append_lightclient_candidate(
        ctx, candidate_xonly, candidate_capacity, &n_out, shared_secret33, recipient_spend_pubkey, 0);
    if (!ret) {
        rustsecp256k1_v0_10_0_memclear(shared_secret33, sizeof(shared_secret33));
        return 0;
    }

    if (label_lookup != NULL) {
        for (i = 0; i < n_label_integers; i++) {
            m = label_integers[i];
            label_ser_len = sizeof(label_ser);
            if (!rustsecp256k1_v0_10_0_silentpayments_recipient_create_label(ctx, &label_pubkey, label_tweak32, recipient_scan_key32, m)) {
                continue;
            }
            if (!rustsecp256k1_v0_10_0_ec_pubkey_serialize(ctx, label_ser, &label_ser_len, &label_pubkey, SECP256K1_EC_COMPRESSED)) {
                continue;
            }
            if (label_lookup(label_ser, label_context) == NULL) {
                continue;
            }
            if (!rustsecp256k1_v0_10_0_silentpayments_recipient_create_labeled_spend_pubkey(ctx, &labeled_spend_pubkey, recipient_spend_pubkey, &label_pubkey)) {
                rustsecp256k1_v0_10_0_memclear(shared_secret33, sizeof(shared_secret33));
                return 0;
            }
            ret = rustsecp256k1_v0_10_0_silentpayments_append_lightclient_candidate(
                ctx, candidate_xonly, candidate_capacity, &n_out, shared_secret33, &labeled_spend_pubkey, 0);
            if (!ret) {
                rustsecp256k1_v0_10_0_memclear(shared_secret33, sizeof(shared_secret33));
                return 0;
            }
            rustsecp256k1_v0_10_0_memclear(label_tweak32, sizeof(label_tweak32));
        }
    }

    rustsecp256k1_v0_10_0_memclear(shared_secret33, sizeof(shared_secret33));
    *n_candidates = n_out;
    return 1;
}

int rustsecp256k1_v0_10_0_silentpayments_recipient_create_output_pubkey(const rustsecp256k1_v0_10_0_context *ctx, rustsecp256k1_v0_10_0_xonly_pubkey *P_output_xonly, const unsigned char *shared_secret33, const rustsecp256k1_v0_10_0_pubkey *recipient_spend_pubkey, const uint32_t k)
{
    VERIFY_CHECK(ctx != NULL);
    ARG_CHECK(P_output_xonly != NULL);
    ARG_CHECK(shared_secret33 != NULL);
    ARG_CHECK(recipient_spend_pubkey != NULL);
    return rustsecp256k1_v0_10_0_silentpayments_create_output_pubkey(ctx, P_output_xonly, shared_secret33, recipient_spend_pubkey, k);
}


#endif

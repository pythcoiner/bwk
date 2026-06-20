/***********************************************************************
 * Copyright (c) 2024 josibake                                         *
 * Distributed under the MIT software license, see the accompanying    *
 * file COPYING or https://www.opensource.org/licenses/mit-license.php.*
 ***********************************************************************/

#ifndef SECP256K1_MODULE_SILENTPAYMENTS_BENCH_H
#define SECP256K1_MODULE_SILENTPAYMENTS_BENCH_H

#include "../../../include/secp256k1_silentpayments.h"

#define LIGHTCLIENT_LABEL_COUNT 4
#define LIGHTCLIENT_MAX_CANDIDATES 8

struct label_cache_entry {
    unsigned char label[33];
    unsigned char label_tweak[32];
};

struct labels_cache {
    size_t entries_used;
    struct label_cache_entry entries[LIGHTCLIENT_LABEL_COUNT];
};

typedef struct {
    rustsecp256k1_v0_10_0_context *ctx;
    rustsecp256k1_v0_10_0_pubkey spend_pubkey;
    rustsecp256k1_v0_10_0_pubkey combined_tweak_pubkey;
    unsigned char scan_key[32];
    unsigned char input_pubkey33[33];
    rustsecp256k1_v0_10_0_xonly_pubkey tx_outputs[2];
    rustsecp256k1_v0_10_0_xonly_pubkey tx_inputs[2];
    rustsecp256k1_v0_10_0_silentpayments_found_output found_outputs[2];
    rustsecp256k1_v0_10_0_xonly_pubkey lightclient_candidates[LIGHTCLIENT_MAX_CANDIDATES];
    unsigned char scalar[32];
    unsigned char smallest_outpoint[36];
    struct labels_cache labels_cache;
    uint32_t lightclient_label_integers[LIGHTCLIENT_LABEL_COUNT];
    size_t lightclient_n_label_integers;
} bench_silentpayments_data;

static struct labels_cache label_cache;
const unsigned char* label_lookup(const unsigned char* key, const void* cache_ptr) {
    const struct labels_cache* cache = (const struct labels_cache*)cache_ptr;
    size_t i;

    if (cache == NULL) {
        return NULL;
    }
    for (i = 0; i < cache->entries_used; i++) {
        if (rustsecp256k1_v0_10_0_memcmp_var(cache->entries[i].label, key, 33) == 0) {
            return cache->entries[i].label_tweak;
        }
    }
    return NULL;
}

static void bench_silentpayments_scan_setup(void* arg) {
    int i;
    bench_silentpayments_data *data = (bench_silentpayments_data*)arg;
    const unsigned char tx_outputs[2][32] = {
        {0x84,0x17,0x92,0xc3,0x3c,0x9d,0xc6,0x19,0x3e,0x76,0x74,0x41,0x34,0x12,0x5d,0x40,0xad,0xd8,0xf2,0xf4,0xa9,0x64,0x75,0xf2,0x8b,0xa1,0x50,0xbe,0x03,0x2d,0x64,0xe8},
        {0x2e,0x84,0x7b,0xb0,0x1d,0x1b,0x49,0x1d,0xa5,0x12,0xdd,0xd7,0x60,0xb8,0x50,0x96,0x17,0xee,0x38,0x05,0x70,0x03,0xd6,0x11,0x5d,0x00,0xba,0x56,0x24,0x51,0x32,0x3a},
    };
    const unsigned char static_tx_input[32] = {
        0xf2,0x07,0x16,0x2b,0x1a,0x7a,0xbc,0x51,
        0xc4,0x20,0x17,0xbe,0xf0,0x55,0xe9,0xec,
        0x1e,0xfc,0x3d,0x35,0x67,0xcb,0x72,0x03,
        0x57,0xe2,0xb8,0x43,0x25,0xdb,0x33,0xac
    };
    const unsigned char smallest_outpoint[36] = {
        0x16, 0x9e, 0x1e, 0x83, 0xe9, 0x30, 0x85, 0x33, 0x91,
        0xbc, 0x6f, 0x35, 0xf6, 0x05, 0xc6, 0x75, 0x4c, 0xfe,
        0xad, 0x57, 0xcf, 0x83, 0x87, 0x63, 0x9d, 0x3b, 0x40,
        0x96, 0xc5, 0x4f, 0x18, 0xf4, 0x00, 0x00, 0x00, 0x00,
    };
    const unsigned char spend_pubkey[33] = {
        0x02,0xee,0x97,0xdf,0x83,0xb2,0x54,0x6a,
        0xf5,0xa7,0xd0,0x62,0x15,0xd9,0x8b,0xcb,
        0x63,0x7f,0xe0,0x5d,0xd0,0xfa,0x37,0x3b,
        0xd8,0x20,0xe6,0x64,0xd3,0x72,0xde,0x9a,0x01
    };
    const unsigned char scan_key[32] = {
        0xa8,0x90,0x54,0xc9,0x5b,0xe3,0xc3,0x01,
        0x56,0x65,0x74,0xf2,0xaa,0x93,0xad,0xe0,
        0x51,0x85,0x09,0x03,0xa6,0x9c,0xbd,0xd1,
        0xd4,0x7e,0xae,0x26,0x3d,0x7b,0xc0,0x31
    };
    rustsecp256k1_v0_10_0_keypair input_keypair;
    rustsecp256k1_v0_10_0_pubkey input_pubkey;
    size_t pubkeylen = 33;

    for (i = 0; i < 32; i++) {
        data->scalar[i] = i + 1;
    }
    for (i = 0; i < 2; i++) {
        CHECK(rustsecp256k1_v0_10_0_xonly_pubkey_parse(data->ctx, &data->tx_outputs[i], tx_outputs[i]));
    }
    /* Create the first input public key from the scalar.
     * This input is also used to create the serialized public data object for the light client
     */
    CHECK(rustsecp256k1_v0_10_0_keypair_create(data->ctx, &input_keypair, data->scalar));
    CHECK(rustsecp256k1_v0_10_0_keypair_pub(data->ctx, &input_pubkey, &input_keypair));
    CHECK(rustsecp256k1_v0_10_0_ec_pubkey_serialize(data->ctx, data->input_pubkey33, &pubkeylen, &input_pubkey, SECP256K1_EC_COMPRESSED));
    CHECK(rustsecp256k1_v0_10_0_ec_pubkey_parse(data->ctx, &data->combined_tweak_pubkey, data->input_pubkey33, pubkeylen));
    /* Create the input public keys for the full scan */
    CHECK(rustsecp256k1_v0_10_0_keypair_xonly_pub(data->ctx, &data->tx_inputs[0], NULL, &input_keypair));
    CHECK(rustsecp256k1_v0_10_0_xonly_pubkey_parse(data->ctx, &data->tx_inputs[1], static_tx_input));
    CHECK(rustsecp256k1_v0_10_0_ec_pubkey_parse(data->ctx, &data->spend_pubkey, spend_pubkey, pubkeylen));
    memcpy(data->scan_key, scan_key, 32);
    memcpy(data->smallest_outpoint, smallest_outpoint, 36);

    data->labels_cache.entries_used = LIGHTCLIENT_LABEL_COUNT;
    data->lightclient_n_label_integers = LIGHTCLIENT_LABEL_COUNT;
    for (i = 0; i < LIGHTCLIENT_LABEL_COUNT; i++) {
        unsigned int label_integer = (unsigned int)(i + 1);
        struct label_cache_entry *cache_entry = &data->labels_cache.entries[i];

        data->lightclient_label_integers[i] = label_integer;
        CHECK(rustsecp256k1_v0_10_0_silentpayments_recipient_create_label(data->ctx,
            &input_pubkey,
            cache_entry->label_tweak,
            data->scan_key,
            label_integer));
        pubkeylen = 33;
        CHECK(rustsecp256k1_v0_10_0_ec_pubkey_serialize(data->ctx, cache_entry->label, &pubkeylen, &input_pubkey, SECP256K1_EC_COMPRESSED));
    }
}

static void bench_silentpayments_output_scan(void* arg, int iters) {
    int i, k = 0;
    bench_silentpayments_data *data = (bench_silentpayments_data*)arg;
    rustsecp256k1_v0_10_0_silentpayments_recipient_public_data public_data;

    for (i = 0; i < iters; i++) {
        unsigned char shared_secret[33];
        rustsecp256k1_v0_10_0_xonly_pubkey xonly_output;
        CHECK(rustsecp256k1_v0_10_0_silentpayments_recipient_public_data_parse(data->ctx, &public_data, data->input_pubkey33));
        CHECK(rustsecp256k1_v0_10_0_silentpayments_recipient_create_shared_secret(data->ctx,
            shared_secret,
            data->scan_key,
            &public_data
        ));
        CHECK(rustsecp256k1_v0_10_0_silentpayments_recipient_create_output_pubkey(data->ctx,
            &xonly_output,
            shared_secret,
            &data->spend_pubkey,
            k
        ));
    }
}

static void bench_silentpayments_full_tx_scan(void* arg, int iters) {
    int i;
    size_t n_found = 0;
    rustsecp256k1_v0_10_0_silentpayments_found_output *found_output_ptrs[2];
    const rustsecp256k1_v0_10_0_xonly_pubkey *tx_output_ptrs[2];
    const rustsecp256k1_v0_10_0_xonly_pubkey *tx_input_ptrs[2];
    bench_silentpayments_data *data = (bench_silentpayments_data*)arg;
    rustsecp256k1_v0_10_0_silentpayments_recipient_public_data public_data;

    for (i = 0; i < 2; i++) {
        found_output_ptrs[i] = &data->found_outputs[i];
        tx_output_ptrs[i] = &data->tx_outputs[i];
        tx_input_ptrs[i] = &data->tx_inputs[i];
    }
    for (i = 0; i < iters; i++) {
        CHECK(rustsecp256k1_v0_10_0_silentpayments_recipient_public_data_create(data->ctx,
            &public_data,
            data->smallest_outpoint,
            tx_input_ptrs, 2,
            NULL, 0
        ));
        CHECK(rustsecp256k1_v0_10_0_silentpayments_recipient_scan_outputs(data->ctx,
            found_output_ptrs, &n_found,
            tx_output_ptrs, 2,
            data->scan_key,
            &public_data,
            &data->spend_pubkey,
            label_lookup, &label_cache)
        );
    }
}

static void bench_silentpayments_lightclient_scan_no_labels(void* arg, int iters) {
    int i;
    size_t n_candidates;
    bench_silentpayments_data *data = (bench_silentpayments_data*)arg;

    for (i = 0; i < iters; i++) {
        n_candidates = LIGHTCLIENT_MAX_CANDIDATES;
        CHECK(rustsecp256k1_v0_10_0_silentpayments_recipient_scan_lightclient(data->ctx,
            data->lightclient_candidates,
            &n_candidates,
            &data->combined_tweak_pubkey,
            data->scan_key,
            &data->spend_pubkey,
            NULL,
            NULL,
            NULL,
            0));
    }
}

static void bench_silentpayments_lightclient_scan_labels(void* arg, int iters) {
    int i;
    size_t n_candidates;
    bench_silentpayments_data *data = (bench_silentpayments_data*)arg;

    for (i = 0; i < iters; i++) {
        n_candidates = LIGHTCLIENT_MAX_CANDIDATES;
        CHECK(rustsecp256k1_v0_10_0_silentpayments_recipient_scan_lightclient(data->ctx,
            data->lightclient_candidates,
            &n_candidates,
            &data->combined_tweak_pubkey,
            data->scan_key,
            &data->spend_pubkey,
            label_lookup,
            &data->labels_cache,
            data->lightclient_label_integers,
            data->lightclient_n_label_integers));
    }
}

static void run_silentpayments_bench(int iters, int argc, char** argv) {
    bench_silentpayments_data data;
    int d = argc == 1;

    /* create a context with no capabilities */
    data.ctx = rustsecp256k1_v0_10_0_context_create(SECP256K1_FLAGS_TYPE_CONTEXT);

    if (d || have_flag(argc, argv, "silentpayments")) run_benchmark("silentpayments_full_tx_scan", bench_silentpayments_full_tx_scan, bench_silentpayments_scan_setup, NULL, &data, 10, iters);
    if (d || have_flag(argc, argv, "silentpayments")) run_benchmark("silentpayments_output_scan", bench_silentpayments_output_scan, bench_silentpayments_scan_setup, NULL, &data, 10, iters);
    if (d || have_flag(argc, argv, "silentpayments") || have_flag(argc, argv, "silentpayments_lightclient_scan")) run_benchmark("silentpayments_lightclient_scan_no_labels", bench_silentpayments_lightclient_scan_no_labels, bench_silentpayments_scan_setup, NULL, &data, 10, iters);
    if (d || have_flag(argc, argv, "silentpayments") || have_flag(argc, argv, "silentpayments_lightclient_scan")) run_benchmark("silentpayments_lightclient_scan_labels", bench_silentpayments_lightclient_scan_labels, bench_silentpayments_scan_setup, NULL, &data, 10, iters);

    rustsecp256k1_v0_10_0_context_destroy(data.ctx);
}

#endif /* SECP256K1_MODULE_SILENTPAYMENTS_BENCH_H */

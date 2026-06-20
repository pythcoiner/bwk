/***********************************************************************
 * bwk-spscan-sys byte-based FFI shim.                                  *
 *                                                                      *
 * Wraps the two light-client SP scan kernels behind a byte-only ABI so *
 * the Rust caller needs no secp struct layout: pubkeys cross as 33-byte *
 * compressed blobs, candidates come back as 32-byte x-only keys.       *
 *                                                                      *
 * These mirror the exact ordering/semantics of the rust-secp wrappers  *
 * in secp256k1/src/silentpayments.rs so the output is byte-identical.  *
 ***********************************************************************/

#include <stddef.h>
#include <stdlib.h>

#include "../../../include/secp256k1.h"
#include "../../../include/secp256k1_extrakeys.h"
#include "../../../include/secp256k1_silentpayments.h"

/* Parse n 33-byte compressed pubkeys into the out array. Returns 1 on
 * success, 0 if any pubkey is malformed (fail-loud: a bad pubkey is a
 * caller bug, not something to silently skip). */
static int spscan_parse_pubkeys(
    const bwkspscan_v0_1_0_context *ctx,
    bwkspscan_v0_1_0_pubkey *out,
    const unsigned char *blobs33,
    size_t n
) {
    size_t i;
    for (i = 0; i < n; i++) {
        if (!bwkspscan_v0_1_0_ec_pubkey_parse(ctx, &out[i], blobs33 + i * 33, 33)) {
            return 0;
        }
    }
    return 1;
}

/* Serialize n x-only pubkeys to 32 bytes each, packed into out_xonly32. */
static void spscan_serialize_xonly(
    const bwkspscan_v0_1_0_context *ctx,
    unsigned char *out_xonly32,
    const bwkspscan_v0_1_0_xonly_pubkey *xonly,
    size_t n
) {
    size_t i;
    for (i = 0; i < n; i++) {
        bwkspscan_v0_1_0_xonly_pubkey_serialize(ctx, out_xonly32 + i * 32, &xonly[i]);
    }
}

/* Per-tweak entry. Parses spend_points33 (n_spend_points x 33 bytes) and the
 * single combined tweak (tweak33), runs the per-tweak kernel, and writes
 * n_spend_points x-only candidates (32 bytes each) to out_xonly32.
 * *n_out is set to the number of candidates written.
 * Returns 1 on success, 0 on any parse/kernel failure. */
int bwkspscan_scan_spend_points(
    const bwkspscan_v0_1_0_context *ctx,
    unsigned char *out_xonly32,
    size_t *n_out,
    const unsigned char *tweak33,
    const unsigned char *scan_key32,
    const unsigned char *spend_points33,
    size_t n_spend_points
) {
    bwkspscan_v0_1_0_pubkey tweak;
    bwkspscan_v0_1_0_pubkey spend_points[64];
    bwkspscan_v0_1_0_xonly_pubkey candidates[64];
    size_t n_candidates = n_spend_points;

    if (n_spend_points == 0 || n_spend_points > 64) {
        return 0;
    }
    if (!bwkspscan_v0_1_0_ec_pubkey_parse(ctx, &tweak, tweak33, 33)) {
        return 0;
    }
    if (!spscan_parse_pubkeys(ctx, spend_points, spend_points33, n_spend_points)) {
        return 0;
    }
    if (!bwkspscan_v0_1_0_silentpayments_recipient_scan_lightclient_spend_points(
            ctx, candidates, &n_candidates, &tweak, scan_key32,
            spend_points, n_spend_points)) {
        return 0;
    }
    spscan_serialize_xonly(ctx, out_xonly32, candidates, n_candidates);
    *n_out = n_candidates;
    return 1;
}

/* Batch entry. Parses n_tweaks tweaks (tweaks33) and n_spend_points spend
 * points, runs the batched kernel, and writes n_tweaks * n_spend_points
 * x-only candidates (32 bytes each) to out_xonly32, grouped by tweak (the
 * candidate for tweak t and spend point s is at index t*n_spend_points + s).
 * *n_out is set to the number of candidates written.
 * Returns 1 on success, 0 on any parse/kernel failure. */
int bwkspscan_scan_spend_points_batch(
    const bwkspscan_v0_1_0_context *ctx,
    unsigned char *out_xonly32,
    size_t *n_out,
    const unsigned char *tweaks33,
    size_t n_tweaks,
    const unsigned char *scan_key32,
    const unsigned char *spend_points33,
    size_t n_spend_points
) {
    /* Heap-allocate the parsed-pubkey and candidate buffers since the batch
     * count is unbounded; the kernel itself phases work in fixed chunks. */
    bwkspscan_v0_1_0_pubkey *tweaks = NULL;
    bwkspscan_v0_1_0_pubkey *spend_points = NULL;
    bwkspscan_v0_1_0_xonly_pubkey *candidates = NULL;
    size_t total;
    size_t n_candidates;
    int ret = 0;

    if (n_tweaks == 0 || n_spend_points == 0) {
        *n_out = 0;
        return 1;
    }

    total = n_tweaks * n_spend_points;
    n_candidates = total;
    tweaks = malloc(n_tweaks * sizeof(*tweaks));
    spend_points = malloc(n_spend_points * sizeof(*spend_points));
    candidates = malloc(total * sizeof(*candidates));
    if (tweaks == NULL || spend_points == NULL || candidates == NULL) {
        goto done;
    }

    if (!spscan_parse_pubkeys(ctx, tweaks, tweaks33, n_tweaks)) {
        goto done;
    }
    if (!spscan_parse_pubkeys(ctx, spend_points, spend_points33, n_spend_points)) {
        goto done;
    }
    if (!bwkspscan_v0_1_0_silentpayments_recipient_scan_lightclient_spend_points_batch(
            ctx, candidates, &n_candidates, tweaks, n_tweaks, scan_key32,
            spend_points, n_spend_points)) {
        goto done;
    }
    spscan_serialize_xonly(ctx, out_xonly32, candidates, n_candidates);
    *n_out = n_candidates;
    ret = 1;

done:
    free(tweaks);
    free(spend_points);
    free(candidates);
    return ret;
}

/* Byte-only FFI shim for the SP scan kernels. */

#include <stddef.h>
#include <stdlib.h>

#include "secp256k1.h"
#include "secp256k1_extrakeys.h"
#include "secp256k1_silentpayments.h"

/* Parse n 33-byte compressed pubkeys into the out array. Returns 1 on
 * success, 0 if any pubkey is malformed. */
static int spscan_parse_pubkeys(
    const secp256k1_context *ctx,
    secp256k1_pubkey *out,
    const unsigned char *blobs33,
    size_t n
) {
    size_t i;
    for (i = 0; i < n; i++) {
        if (!secp256k1_ec_pubkey_parse(ctx, &out[i], blobs33 + i * 33, 33)) {
            return 0;
        }
    }
    return 1;
}

/* Serialize n x-only pubkeys to 32 bytes each, packed into out_xonly32. */
static void spscan_serialize_xonly(
    const secp256k1_context *ctx,
    unsigned char *out_xonly32,
    const secp256k1_xonly_pubkey *xonly,
    size_t n
) {
    size_t i;
    for (i = 0; i < n; i++) {
        secp256k1_xonly_pubkey_serialize(ctx, out_xonly32 + i * 32, &xonly[i]);
    }
}

/* Per-tweak entry. Parses spend_points33 and tweak33, runs the per-tweak
 * kernel, and writes n_spend_points x-only candidates to out_xonly32. */
int secp256k1_spscan_scan_spend_points(
    const secp256k1_context *ctx,
    unsigned char *out_xonly32,
    size_t *n_out,
    const unsigned char *tweak33,
    const unsigned char *scan_key32,
    const unsigned char *spend_points33,
    size_t n_spend_points
) {
    secp256k1_pubkey tweak;
    secp256k1_pubkey spend_points[64];
    secp256k1_xonly_pubkey candidates[64];
    size_t n_candidates = n_spend_points;

    if (n_spend_points == 0 || n_spend_points > 64) {
        return 0;
    }
    if (!secp256k1_ec_pubkey_parse(ctx, &tweak, tweak33, 33)) {
        return 0;
    }
    if (!spscan_parse_pubkeys(ctx, spend_points, spend_points33, n_spend_points)) {
        return 0;
    }
    if (!secp256k1_silentpayments_recipient_scan_lightclient_spend_points(
            ctx, candidates, &n_candidates, &tweak, scan_key32,
            spend_points, n_spend_points)) {
        return 0;
    }
    spscan_serialize_xonly(ctx, out_xonly32, candidates, n_candidates);
    *n_out = n_candidates;
    return 1;
}

/* Batch entry. Parses n_tweaks tweaks and n_spend_points spend points, runs the
 * batched kernel, and writes candidates grouped by tweak to out_xonly32. */
int secp256k1_spscan_scan_spend_points_batch(
    const secp256k1_context *ctx,
    unsigned char *out_xonly32,
    size_t *n_out,
    const unsigned char *tweaks33,
    size_t n_tweaks,
    const unsigned char *scan_key32,
    const unsigned char *spend_points33,
    size_t n_spend_points
) {
    secp256k1_pubkey *tweaks = NULL;
    secp256k1_pubkey *spend_points = NULL;
    secp256k1_xonly_pubkey *candidates = NULL;
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
    if (!secp256k1_silentpayments_recipient_scan_lightclient_spend_points_batch(
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

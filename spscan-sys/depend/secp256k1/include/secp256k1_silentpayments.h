#ifndef SECP256K1_SILENTPAYMENTS_H
#define SECP256K1_SILENTPAYMENTS_H

#include <stdint.h>
#include "secp256k1.h"
#include "secp256k1_extrakeys.h"

#ifdef __cplusplus
extern "C" {
#endif

/** Silent Payments (BIP352), STRIPPED to the light-client scan path.
 *
 *  This vendored copy keeps only the two light-client candidate-output kernels
 *  used by bwk-spscan-sys. The sender path, full-node recipient scan, public
 *  data (de)serialization, label helpers and the base scan_lightclient entry
 *  point were removed along with their types. See depend/README.
 */

/** Create Silent Payment candidate outputs over precomputed spend points.
 *
 *  Given the combined tweak `T`, the recipient's 32 byte scan key, and the
 *  recipient's precomputed spend points (the unlabeled spend pubkey first, then
 *  one `spend_pubkey + label_point` per label), derive the `k = 0` candidate
 *  x-only output key for each spend point in a single call.
 *
 *  This computes the shared secret once, derives `t_0`, computes `t_0 * G` once,
 *  and adds that point to each precomputed spend point. The candidate order
 *  matches the spend point order.
 *
 *  The caller supplies the output buffer and `n_candidates` as the buffer
 *  capacity on input. On success, `n_candidates` is updated with the number of
 *  candidates written (equal to `n_spend_points`).
 *
 *  This runs in variable time over the recipient's secret scan key, so it must
 *  only be used for recipient scanning, never in a signing context.
 *
 *  Returns: 1 if candidate output creation was successful. 0 if an error occurred.
 *  Args:                  ctx: pointer to a context object
 *  Out:       candidate_xonly: pointer to the resulting candidate x-only pubkeys
 *       n_candidates: pointer to the buffer capacity on input and the number of
 *                     candidates written on output
 *  In:          combined_tweak: pointer to the combined tweak pubkey T
 *                  scan_key32: pointer to the recipient's 32 byte scan key
 *                 spend_points: pointer to the precomputed spend points
 *               n_spend_points: the number of spend points
 */
SECP256K1_API SECP256K1_WARN_UNUSED_RESULT int bwkspscan_v0_1_0_silentpayments_recipient_scan_lightclient_spend_points(
    const bwkspscan_v0_1_0_context *ctx,
    bwkspscan_v0_1_0_xonly_pubkey *candidate_xonly,
    size_t *n_candidates,
    const bwkspscan_v0_1_0_pubkey *combined_tweak,
    const unsigned char *scan_key32,
    const bwkspscan_v0_1_0_pubkey *spend_points,
    size_t n_spend_points
) SECP256K1_ARG_NONNULL(1) SECP256K1_ARG_NONNULL(2) SECP256K1_ARG_NONNULL(3) SECP256K1_ARG_NONNULL(4) SECP256K1_ARG_NONNULL(5) SECP256K1_ARG_NONNULL(6);

/** Create Silent Payment candidate outputs for many tweaks in one call.
 *
 *  Batched form of `_recipient_scan_lightclient_spend_points`. Given the
 *  recipient's 32 byte scan key, the recipient's precomputed spend points, and an
 *  array of combined tweaks `T`, derive the `k = 0` candidate x-only output key
 *  for every (tweak, spend point) pair. The output is grouped by tweak: candidate
 *  for tweak `t` and spend point `s` is written to
 *  `out_xonly[t * n_spend_points + s]`. The result is byte-identical to calling
 *  the single-tweak primitive once per tweak.
 *
 *  Internally the work is phased over fixed-size chunks of tweaks so the per-chunk
 *  field inversions are batched (one inversion per chunk for the shared secrets,
 *  one per bounded candidate sub-chunk) instead of one inversion per tweak.
 *
 *  The caller supplies the output buffer and `n_out` as the buffer capacity on
 *  input; it must be at least `n_tweaks * n_spend_points`. On success `n_out` is
 *  updated with the number of candidates written (`n_tweaks * n_spend_points`).
 *
 *  This runs in variable time over the recipient's secret scan key, so it must
 *  only be used for recipient scanning, never in a signing context.
 *
 *  Returns: 1 if candidate output creation was successful. 0 if an error occurred.
 *  Args:                  ctx: pointer to a context object
 *  Out:             out_xonly: pointer to the resulting candidate x-only pubkeys
 *                       n_out: pointer to the buffer capacity on input and the
 *                              number of candidates written on output
 *  In:                 tweaks: pointer to the combined tweak pubkeys
 *                    n_tweaks: the number of tweaks
 *                  scan_key32: pointer to the recipient's 32 byte scan key
 *                spend_points: pointer to the precomputed spend points
 *              n_spend_points: the number of spend points
 */
SECP256K1_API SECP256K1_WARN_UNUSED_RESULT int bwkspscan_v0_1_0_silentpayments_recipient_scan_lightclient_spend_points_batch(
    const bwkspscan_v0_1_0_context *ctx,
    bwkspscan_v0_1_0_xonly_pubkey *out_xonly,
    size_t *n_out,
    const bwkspscan_v0_1_0_pubkey *tweaks,
    size_t n_tweaks,
    const unsigned char *scan_key32,
    const bwkspscan_v0_1_0_pubkey *spend_points,
    size_t n_spend_points
) SECP256K1_ARG_NONNULL(1) SECP256K1_ARG_NONNULL(2) SECP256K1_ARG_NONNULL(3) SECP256K1_ARG_NONNULL(4) SECP256K1_ARG_NONNULL(6) SECP256K1_ARG_NONNULL(7);

#ifdef __cplusplus
}
#endif

#endif /* SECP256K1_SILENTPAYMENTS_H */

// SPDX-License-Identifier: CC0-1.0

//! Support for BIP352 Silent Payments light-client scanning.
//!

use alloc::vec::Vec;
use core::ptr;

use secp256k1_sys::types::{c_uchar, c_void};

use crate::ffi::{self, CPtr};
use crate::{Error, PublicKey, Secp256k1, SecretKey, Signing, Verification, XOnlyPublicKey};

/// Looks up the tweak for a label public key in the recipient's label cache.
///
/// Given a label public key in compressed (33-byte) form, returns the 32-byte
/// label tweak if the label is in the cache, or `None` otherwise. Only the
/// presence of the label is consulted while building light-client candidates;
/// the tweak bytes are not read.
pub trait LabelLookup {
    /// Returns the tweak for `label33` if the label is known, else `None`.
    fn lookup(&self, label33: &[u8; 33]) -> Option<&[u8; 32]>;
}

/// Builds the candidate Silent Payment output keys a light client should test
/// against its block filter.
///
/// Given the combined tweak `T`, the recipient's scan key and spend public key,
/// returns the candidate x-only output keys: the unlabeled `k = 0` output first,
/// followed by one labeled variant per requested label integer that `labels`
/// reports as present in the recipient's cache.
///
/// This is a recipient-scan helper only: it derives candidates the caller tests
/// against a filter, it does NOT match against transaction outputs. It runs in
/// variable time over the recipient's secret scan key and so must not be used in
/// a context where timing is observable by an adversary.
///
/// The no-label path (`labels = None`) is the common case and needs no callback.
pub fn recipient_scan_lightclient<C: Verification + Signing>(
    secp: &Secp256k1<C>,
    combined_tweak: &PublicKey,
    scan_key: &SecretKey,
    spend_pubkey: &PublicKey,
    labels: Option<(&[u32], &dyn LabelLookup)>,
) -> Result<Vec<XOnlyPublicKey>, Error> {
    let (label_integers, lookup) = match labels {
        Some((integers, lookup)) => (integers, Some(lookup)),
        None => (&[][..], None),
    };
    let capacity = 1 + label_integers.len();

    // The buffer is written by the C function as an out-parameter; the zeroed
    // entries are never read by it.
    let mut candidates: Vec<ffi::XOnlyPublicKey> =
        (0..capacity).map(|_| unsafe { ffi::XOnlyPublicKey::new() }).collect();
    let mut n_candidates = capacity;

    // `lookup` must outlive the FFI call: `label_context` points at this binding.
    let (callback, label_context, integers_ptr): (ffi::SilentpaymentsLabelLookup, _, _) =
        match &lookup {
            Some(lookup) => (
                Some(label_lookup_trampoline),
                lookup as *const &dyn LabelLookup as *const c_void,
                label_integers.as_ptr(),
            ),
            None => (None, ptr::null(), ptr::null()),
        };

    let ret = unsafe {
        ffi::secp256k1_silentpayments_recipient_scan_lightclient(
            secp.ctx.as_ptr(),
            candidates.as_mut_ptr(),
            &mut n_candidates,
            combined_tweak.as_c_ptr(),
            scan_key.as_c_ptr(),
            spend_pubkey.as_c_ptr(),
            callback,
            label_context,
            integers_ptr,
            label_integers.len(),
        )
    };

    if ret != 1 {
        return Err(Error::InvalidPublicKey);
    }

    candidates.truncate(n_candidates);
    Ok(candidates.into_iter().map(XOnlyPublicKey::from).collect())
}

/// C callback bridging the [`LabelLookup`] trait to the FFI.
///
/// `label_context` is a pointer to a `&dyn LabelLookup`. The returned pointer
/// borrows the tweak owned by the lookup and stays valid for the duration of the
/// FFI call.
unsafe extern "C" fn label_lookup_trampoline(
    label33: *const c_uchar,
    label_context: *const c_void,
) -> *const c_uchar {
    let lookup = *(label_context as *const &dyn LabelLookup);
    let label33 = &*(label33 as *const [u8; 33]);
    match lookup.lookup(label33) {
        Some(tweak) => tweak.as_ptr(),
        None => ptr::null(),
    }
}

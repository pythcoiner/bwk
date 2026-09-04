//! Tests for `Account::all_coins` and `Account::all_spendable_coins`.
//!
//! Real-coin scenarios across SP + sub-accounts run as part of the regtest
//! integration suite (gated by `BWK_SP_INTEGRATION_TEST=1`); these tests cover
//! the empty-account aggregation path and the type-level shape of the
//! returned views.

mod common;

use bitcoin::Amount;
use bwk_sp::account::unified::{CoinOrigin, SpendableSummary};
use common::test_account_named;

#[test]
fn all_coins_empty_account() {
    let account = test_account_named("unified-empty", "http://127.0.0.1:1");

    let coins = account.all_coins();
    assert!(coins.is_empty(), "expected no coins, got {coins:?}");
}

#[test]
fn all_spendable_coins_empty_account() {
    let account = test_account_named("unified-spendable-empty", "http://127.0.0.1:1");

    let summary = account.all_spendable_coins();
    assert_eq!(
        summary,
        SpendableSummary {
            confirmed_count: 0,
            confirmed_balance: Amount::ZERO,
            unconfirmed_count: 0,
            unconfirmed_balance: Amount::ZERO,
        },
    );
}

#[test]
fn coin_origin_eq_and_copy() {
    // Sanity check on CoinOrigin's derived impls: bindings rely on these.
    let a = CoinOrigin::Sp;
    let b = CoinOrigin::SubAccount(0);
    let c = CoinOrigin::SubAccount(0);
    assert_ne!(a, b);
    assert_eq!(b, c);
    let _copy = a;
    let _still_usable = a;
}

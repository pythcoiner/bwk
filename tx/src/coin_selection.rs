use std::collections::BTreeMap;

use bitcoin::Weight;
use rand::random_range;

use crate::Coin;

#[derive(Debug, Clone)]
pub struct Selection {
    spendable_amount: u64,
    fees: u64,
    outpoints: Vec<bitcoin::OutPoint>,
}

impl Selection {
    pub fn new(coins: Vec<&Coin>, feerate: u64 /* msats/vb */) -> Self {
        let mut spendable_amount = 0;
        let mut fees = 0;
        let mut outpoints = vec![];
        for c in coins {
            let fee = Weight::from_wu(c.satisfaction_size).to_vbytes_ceil() * feerate / 1000;
            fees += fee;
            spendable_amount += c.txout.value.to_sat() - fee;
            outpoints.push(c.outpoint);
        }

        Selection {
            spendable_amount,
            fees,
            outpoints,
        }
    }
}

pub fn fees(coin: &Coin, feerate: u64 /* msats/vb */) -> u64 {
    Weight::from_wu(coin.satisfaction_size).to_vbytes_ceil() * feerate / 1000
}

// Sort out coins if fees >= spendable value
pub fn discard_dust(coins: Vec<Coin>, feerate: u64 /* msats/vb */) -> Vec<Coin> {
    coins
        .into_iter()
        .filter_map(|c| {
            let fee = fees(&c, feerate);
            (fee * 2 < c.txout.value.to_sat()).then_some(c)
        })
        .collect()
}

pub fn all_coins_combinations(
    coins: Vec<Coin>,
    feerate: u64, /* msats/vb*/
) -> Option<BTreeMap<u64, Selection>> {
    let coins = discard_dust(coins, feerate);

    let n = coins.len();
    if n == 0 || n > 20 {
        // NOTE: >20 coins is too much resources intensive
        return None;
    }

    let mut selections = BTreeMap::new();

    // Try all non-empty subsets: 1 to 2^n - 1
    for mask in 1..(1 << n) {
        let mut selected = Vec::new();
        #[allow(clippy::needless_range_loop)]
        for i in 0..n {
            if (mask & (1 << i)) != 0 {
                selected.push(&coins[i]);
            }
        }

        let sel = Selection::new(selected, feerate);
        selections.insert(sel.spendable_amount, sel);
    }

    Some(selections)
}

pub fn select(
    coins: Vec<Coin>,
    target: u64,
    feerate: u64,
    min_change: u64,
    max_change: u64,
    dust: u64,
) -> (Option<Selection>, Option<Selection>) {
    // NOTE: base tx + output fees must be included in target
    let selections = match all_coins_combinations(coins, feerate) {
        Some(s) => s,
        None => return (None, None),
    };

    // Try to get a selection where we can just drop a tiny change
    let mut selection = if let Some((_, sel)) = selections.range(target..target + dust).next() {
        Some(sel.clone())
    } else {
        None
    };

    let mut cj_selection = None;
    // FIXME: use args for max fee for CJ like
    if target > min_change && feerate < 10_000 {
        // Try to split the input into "equal" outputs amount +-10%
        let chunk_target_min = target * 90 / 100;
        let chunk_target_max = target * 110 / 100;
        let mut cj_like = vec![];
        for i in 2..6 {
            let min = chunk_target_min * i;
            let max = chunk_target_max * i;
            let candidates: Vec<_> = selections.range(min..max).collect();
            if !candidates.is_empty() {
                let index = random_range(0..candidates.len());
                cj_like.push(candidates[index]);
            }
        }
        if !cj_like.is_empty() {
            let index = random_range(0..cj_like.len());
            cj_selection = Some(cj_like[index].1.clone());
        }
    }

    if selection.is_none() {
        // Get the list of selection that match min_change < change < max_change
        let min_change: Vec<_> = selections
            .range(target + min_change..target + max_change)
            .collect();
        if !min_change.is_empty() {
            let index = random_range(0..min_change.len());
            selection = Some(min_change[index].1.clone());
        }
    }

    if selection.is_none() {
        // Not so good choice
        selection = selections
            .range(target + max_change..)
            .next()
            .map(|s| s.1.clone());
    }

    if selection.is_none() {
        // Default choice
        selection = selections.range(target..).next().map(|s| s.1.clone());
    }

    (selection, cj_selection)
}

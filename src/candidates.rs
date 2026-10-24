use crate::chain::Step;
use std::cmp::Reverse;

/// Generate all valid candidate next-steps for a Lucas chain.
///
/// `values` is the chain stored newest-first (values[0] = current top).
/// `steps`  is the full step history; we read the last entry for dbl_count
///          and for detecting whether the previous step was a doubling.
///
/// # Layout invariant
/// `values` is always sorted in **descending** order.
///
/// Membership tests use a **linear scan** rather than `binary_search`. Chains
/// are short (a few dozen entries) and the previous binary search dominated the
/// profile (≈60% of this function) on branch-mispredicting `cmov`/`shr`
/// midpoint arithmetic. A straight descending scan that bails as soon as the
/// probe drops below the sought value is fewer instructions and, because the
/// sought difference is usually large, typically exits after only a few
/// elements near the front.
///
/// Results are written into the caller-provided `out` scratch buffer (cleared
/// on entry) instead of a fresh allocation, eliminating the per-node `Vec`
/// churn that showed up as ~9% malloc/free in the profile.
#[inline]
fn chain_contains(values: &[u64], v: u64) -> bool {
    // values descending: stop once we pass below v.
    for &e in values {
        if e == v { return true; }
        if e < v { return false; }
    }
    false
}

pub fn find_candidates(values: &[u64], steps: &[Step], out: &mut Vec<Step>) {
    out.clear();
    let results = out;
    let top = values[0];
    let len = values.len();
    let last = steps.last().unwrap();
    let current_dbl_count = last.dbl_count;

    // A step is a doubling when both components point to the same index.
    let last_was_doubling = last.comp1 == last.comp2;

    // ── 1. Doublings: 2 * values[i] ──────────────────────────────────────────
    //
    // We now start at i == 0 to include a doubling of the current top (2*top).
    // Previously i started at 1, silently excluding 2*top as a candidate even
    // though it is a perfectly valid differential-addition step (diff == 0 is
    // always in the chain).  The comment in the old code claimed "2*top can
    // never be a valid new top" — this is wrong; 2*top > top, so it is a valid
    // extension.  The C original also generates 2*top here.
    //
    // values is descending, so 2*values[i] shrinks as i grows — break as soon
    // as c <= top since all subsequent i will also fail.
    for (i, &v) in values.iter().enumerate() {
        let c = match v.checked_mul(2) {
            Some(v) => v,
            None => break, // overflow guard
        };
        if c > top {
            results.push(Step {
                value: c,
                comp1: i as u8,
                comp2: i as u8, // comp1 == comp2 signals a doubling
                dbl_count: current_dbl_count + 1,
            });
        } else {
            break;
        }
    }

    // ── 2. Additions: values[i] + values[j],  i < j ──────────────────────────
    //
    // Differential-addition chain rule: (values[i] - values[j]) must already
    // be present in the chain.
    for i in 0..len {
        for j in (i + 1)..len {
            let c = values[i] + values[j];
            if c <= top {
                // values[j] shrinks as j grows; no point continuing for this i.
                break;
            }

            // 2x–3x topology prune (mirrors the C code heuristic):
            // If the previous step was a doubling and this candidate is exactly
            // 3/2 * current_top, the resulting ratio is always suboptimal.
            // Guard: top must be even for the ratio to be integral.
            if last_was_doubling && top.is_multiple_of(2) && c == (top / 2) * 3
                && len > 1 && values[1] * 2 < c {
                    let x = top / 2;
                    let mut compare_val_in_chain = false;
                    for &e in &values[1..] {
                        if e <= x { break; }
                        let compare_val = top - e;
                        if chain_contains(values, compare_val) {
                            compare_val_in_chain = true;
                            break;
                        }
                    }
                    if !compare_val_in_chain {
                        continue;
                    }
                }

            let dif = values[i] - values[j]; // always > 0 since i < j

            if chain_contains(values, dif) {
                // Dedup against what's already queued. `results` is small
                // (a few dozen at most), so a linear scan beats maintaining a
                // separate sorted `seen` Vec with O(n) insert shifts.
                if !results.iter().any(|s| s.value == c) {
                    results.push(Step {
                        value: c,
                        comp1: i as u8,
                        comp2: j as u8,
                        dbl_count: current_dbl_count,
                    });
                }
            }
        }
    }

    // Sort descending so the DFS explores the most-promising (largest)
    // candidates first, and beta-lookahead can reliably treat candidates[0]
    // as the maximal step and candidates[1] as beta.
    results.sort_unstable_by_key(|s| Reverse(s.value));
}

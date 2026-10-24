use crate::chain::{LucasChain, Step};
use crate::registry::ChainRegistry;
use crate::pruner::Pruner;
use crate::candidates::find_candidates;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// A single unit of work: a partially-built chain to be extended by DFS to
/// exactly `target_len` steps.
struct WorkItem {
    chain: LucasChain,
    target_len: u8,
    /// Cancellation flag shared across all items at the same target_len.
    /// When the pass is exhausted (min_target == u64::MAX) any thread sets
    /// this so its siblings drain immediately without doing real work.
    cancelled: Arc<AtomicBool>,
}

/// Expand one generation of seed chains via BFS and return them.
/// `seed_limit` is the target depth; chains already at or past it are
/// returned unchanged.
fn expand_seeds(seeds: Vec<LucasChain>, seed_limit: usize) -> Vec<LucasChain> {
    if seeds.is_empty() { return seeds; }
    // If already deep enough, return as-is.
    if seeds[0].steps.len() >= seed_limit { return seeds; }

    let mut next_gen = Vec::new();
    let mut candidates = Vec::new();
    for chain in seeds {
        find_candidates(&chain.values, &chain.steps, &mut candidates);
        for &cand in &candidates {
            let mut branch = LucasChain {
                values: chain.values.clone(),
                steps:  chain.steps.clone(),
            };
            branch.values.push_front(cand.value);
            branch.steps.push(cand);
            next_gen.push(branch);
        }
    }
    next_gen
}

/// Maximum encodable chain length. The chain code is packed into a u64, and
/// each step costs between 4 and 10 bits, so encode() returns None for any
/// chain that would exceed 64 bits. Searching beyond this length is pointless
/// since no result could ever be stored. In practice all primes at any
/// reasonable B1 are solved well below this ceiling.
const MAX_CHAIN_LEN: u8 = 64;

/// Run the full search for all lengths from 6..=MAX_CHAIN_LEN using a single
/// persistent rayon thread pool fed by a crossbeam work channel.
///
/// We spin up N worker threads that pull WorkItems from a channel in a tight
/// loop. The producer thread generates seeds for length 6, 7, 8, … and sends
/// them into the channel continuously. Workers never idle between passes
/// because by the time they finish length N seeds, length N+1 seeds are
/// already queued. The only synchronisation is the channel itself.
/// Choose the BFS seed depth: the shallowest depth (≥ 3, ≤ `max_depth`) whose
/// number of distinct length-`depth` prefix chains is at least `min_seeds`.
/// This makes parallel work-granularity scale with the thread count while the
/// `max_depth` cap keeps the BFS frontier bounded.
fn pick_seed_depth(min_seeds: usize, max_depth: usize) -> usize {
    let mut depth = 3usize;
    let mut seeds = vec![LucasChain::new()];
    while depth < max_depth {
        let next_depth = depth + 1;
        // expand_seeds targets `seed_limit`; here we grow exactly one level by
        // passing the next depth as the limit.
        seeds = expand_seeds(seeds, next_depth);
        if seeds.is_empty() {
            break; // cannot grow further
        }
        depth = next_depth;
        if seeds.len() >= min_seeds {
            break;
        }
    }
    depth
}

pub fn run_search_all_lengths(
    registry: Arc<ChainRegistry>,
    pruner:   Arc<Pruner>,
    num_threads: usize,
) {
    // ── Seed depth (work granularity) ──────────────────────────────────────
    // Each pass is parallelised by BFS-expanding the standard prefix to
    // `seed_depth` steps, then handing each length-`seed_depth` chain to a
    // worker that DFS-explores it to `target_len`. The number of such seed
    // chains is the task count, so it must comfortably exceed `num_threads` for
    // work-stealing to balance the (very uneven) DFS subtrees.
    //
    // We pick the shallowest depth whose seed count is at least
    // `TASKS_PER_THREAD * num_threads`, capped at `MAX_SEED_DEPTH` so the BFS
    // frontier can never explode (it grows ~2-3x per level). This makes the
    // granularity scale with core count: on a 16-core box it lands at depth 7
    // (~133 seeds, unchanged); on a 128-core box it goes one or two deeper so
    // those cores are not starved of tasks.
    const MAX_SEED_DEPTH: usize = 10;
    const TASKS_PER_THREAD: usize = 8;
    let seed_depth = pick_seed_depth(num_threads * TASKS_PER_THREAD, MAX_SEED_DEPTH);

    // Bounded channel: cap so the producer doesn't race too far ahead and waste
    // memory, but large enough that workers never stall waiting for the producer.
    let (tx, rx) = crossbeam_channel::bounded::<WorkItem>(num_threads * 256);

    // ── Producer thread ───────────────────────────────────────────────────
    // tx is MOVED into the producer closure (not cloned): the channel only
    // closes — and workers' rx.recv() only returns Err — when ALL senders are
    // dropped. Keeping a tx clone alive elsewhere would hang the workers.
    let registry_prod = Arc::clone(&registry);
    let producer = std::thread::spawn(move || {
        for target_len in 6u8..=MAX_CHAIN_LEN {
            if registry_prod.is_complete() { break; }

            let cancelled = Arc::new(AtomicBool::new(false));
            // Can't expand past the pass length itself on short passes.
            let seed_limit = std::cmp::min(target_len as usize, seed_depth);

            let mut seeds = vec![LucasChain::new()];
            for _ in 3..seed_limit {
                seeds = expand_seeds(seeds, seed_limit);
                if seeds.is_empty() { break; }
            }

            for chain in seeds {
                if registry_prod.is_complete() { break; }
                let item = WorkItem {
                    chain,
                    target_len,
                    cancelled: Arc::clone(&cancelled),
                };
                // Blocks if channel is full — natural backpressure.
                if tx.send(item).is_err() { return; }
            }
        }
        // tx drops here, closing the channel.
    });

    // ── Worker pool ───────────────────────────────────────────────────────
    rayon::scope(|s| {
        for _ in 0..num_threads {
            let rx       = &rx;
            let registry = &registry;
            let pruner   = &pruner;
            s.spawn(move |_| {
                // Per-worker candidate-buffer pool, indexed by recursion depth.
                // Each DFS frame reuses its depth's buffer across sibling
                // iterations, so the whole search runs with zero per-node
                // allocation once these have grown to their steady-state size.
                let mut scratch: Vec<Vec<Step>> =
                    (0..=MAX_CHAIN_LEN as usize).map(|_| Vec::with_capacity(32)).collect();
                while let Ok(item) = rx.recv() {
                    let WorkItem { mut chain, target_len, cancelled } = item;
                    dfs_recursive(&mut chain, target_len, registry, pruner, &cancelled, &mut scratch);
                }
            });
        }
    });
    producer.join().expect("producer thread panicked");
}

fn dfs_recursive(
    chain: &mut LucasChain,
    target_len: u8,
    registry: &ChainRegistry,
    pruner: &Pruner,
    cancelled: &AtomicBool,
    scratch: &mut [Vec<Step>],
) {
    if cancelled.load(Ordering::Relaxed) { return; }

    let steps_done = chain.steps.len() as u8;
    if steps_done > target_len { return; }
    let steps_left = target_len - steps_done;

    let c = chain.last_val();
    let p = chain.prev_val();
    let min_target = registry.min_unsolved_prime();

    if min_target == u64::MAX {
        cancelled.store(true, Ordering::Relaxed);
        return;
    }

    if pruner.should_gcd_prune(&chain.values) { return; }
    if !pruner.is_viable(c, p, steps_left, min_target) { return; }

    if steps_left == 0 {
        if pruner.modular_filter(c)
            && let Some(idx) = registry.get_prime_idx(c)
                && let Some(code) = chain.encode() {
                    registry.update_best(
                        idx,
                        target_len,
                        code,
                        chain.steps.last().unwrap().dbl_count,
                    );
                }
        return;
    }

    // Split the buffer pool: this frame fills `head[0]`; recursive children
    // get `tail` so their candidate generation never clobbers ours.
    let (head, tail) = scratch.split_at_mut(1);
    let candidates = &mut head[0];
    find_candidates(&chain.values, &chain.steps, candidates);
    let mut count = candidates.len();
    if count == 0 { return; }

    if count > 1 {
        let max_c_flag = candidates[0].value == c + p;
        if pruner.beta_lookahead_fails(c, p, candidates[1].value, steps_left, min_target, max_c_flag) {
            count = 1;
        }
    }

    // Hoisted viability prune. Profiling showed ~39% of children were recursed
    // into only to be rejected by `is_viable` at their very first check. We
    // apply that same check here, in the parent, before paying for the
    // recursion frame.
    //
    // After taking candidate `v`, the child has top = v, prev = c, and
    // `steps_left - 1` steps remaining. Its maximal Fibonacci growth is
    //   fib[sl]·v + fib[sl-1]·c   (sl = steps_left - 1, i.e. `child_steps`).
    // This is monotonically increasing in `v`. Candidates are sorted
    // descending, so once one fails the lower bound, every later (smaller) one
    // fails too — we `break` instead of merely skipping. (Soundness: identical
    // to the check the child would run; we only avoid the call overhead.)
    let child_steps = steps_left - 1; // steps_left >= 1 here (handled == 0 above)
    for &cand in &candidates[..count] {
        if cancelled.load(Ordering::Relaxed) { return; }

        if cand.value > pruner.target_max {
            // Too big now; smaller candidates may still be valid, so skip
            // (don't break — descending order means the *over* ones are at the
            // front, but a contiguous skip is correct either way).
            continue;
        }
        if !pruner.is_viable(cand.value, c, child_steps, min_target) {
            // Growth bound fails and is monotone in cand.value → all remaining
            // (smaller) candidates also fail.
            break;
        }

        chain.values.push_front(cand.value);
        chain.steps.push(cand);
        dfs_recursive(chain, target_len, registry, pruner, cancelled, tail);
        chain.values.pop_front();
        chain.steps.pop();
    }
}

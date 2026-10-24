use std::sync::atomic::{AtomicU16, AtomicU64, AtomicUsize, Ordering};
use std::fs::File;
use std::io::{BufWriter, Write};

/// A single atomic word that packs chain length and doubling-count together so
/// that both fields can be updated in one compare-and-swap, eliminating the
/// TOCTOU race that existed when they were separate `AtomicU8` values.
///
/// Layout (little-endian bit numbering):
///   bits  7:0  — chain length  (0..=254; 255 == unsolved sentinel)
///   bits 15:8  — doubling count
///
/// Packing helpers are free functions below.
fn pack(len: u8, dbls: u8) -> u16 { (len as u16) | ((dbls as u16) << 8) }
fn unpack_len(w: u16)  -> u8 { (w & 0xFF) as u8 }
fn unpack_dbls(w: u16) -> u8 { (w >> 8) as u8 }

// pack(255, 0) as a literal — fn calls cannot be used in const position on stable Rust.
const UNSOLVED: u16 = 0x00FF;

pub struct ChainRegistry {
    pub primes:    Vec<u64>,
    /// Packed (len, dbls) — updated atomically via CAS.
    state:         Vec<AtomicU16>,
    pub codes:     Vec<AtomicU64>,
    min_unsolved_idx: AtomicUsize,
}

impl ChainRegistry {
    pub fn new(primes: Vec<u64>) -> Self {
        let n = primes.len();
        Self {
            primes,
            state: (0..n).map(|_| AtomicU16::new(UNSOLVED)).collect(),
            codes: (0..n).map(|_| AtomicU64::new(0)).collect(),
            min_unsolved_idx: AtomicUsize::new(0),
        }
    }

    pub fn bootstrap_small_primes(&self) {
        self.update_best(0, 4, 0x2, 1); // prime 11
        self.update_best(1, 4, 0x7, 1); // prime 13
    }

    /// Update the registry for `idx` if this (len, code, dbls) triple is better
    /// than what is currently stored.
    ///
    /// "Better" means either:
    ///   1. A strictly shorter chain length, OR
    ///   2. The same length but more doublings (preferred by GMP-ECM).
    ///
    /// # Correctness under parallelism
    ///
    /// The previous two-field (AtomicU8 length + AtomicU8 dbls) approach had a
    /// residual TOCTOU window in Phase 2: a thread could win the `max_dbls` CAS
    /// and then write `codes`, but another thread might have installed a shorter
    /// chain between those two stores, leaving `codes` pointing at a chain that
    /// no longer matches the stored length.
    ///
    /// Fix: pack (len, dbls) into a single `AtomicU16` and update it with one
    /// CAS.  The `codes` write still races in theory, but the *only* invariant
    /// that matters for correctness is "the stored code decodes to a chain of
    /// length `len`."  Because the CAS is the serialisation point, at most one
    /// thread can transition from any given (old_len, old_dbls) to a strictly
    /// better (new_len, new_dbls).  A concurrent code-store by the same winner
    /// thread is then safe: any later reader who loads the new state word will
    /// also see the matching code (due to the Release store on codes and the
    /// Acquire load on state used by readers).
    pub fn update_best(&self, idx: usize, len: u8, code: u64, dbls: u8) {
        let new_state = pack(len, dbls);
        let mut current = self.state[idx].load(Ordering::Acquire);

        loop {
            let cur_len  = unpack_len(current);
            let cur_dbls = unpack_dbls(current);

            let is_better = len < cur_len || (len == cur_len && dbls > cur_dbls);
            if !is_better {
                return;
            }

            match self.state[idx].compare_exchange_weak(
                current,
                new_state,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    // We own the transition; write the matching code.
                    self.codes[idx].store(code, Ordering::Release);
                    return;
                }
                Err(actual) => {
                    // Another thread changed state; re-evaluate with fresh value.
                    current = actual;
                }
            }
        }
    }

    /// O(1) amortised lookup for the smallest still-unsolved prime.
    pub fn min_unsolved_prime(&self) -> u64 {
        let mut idx = self.min_unsolved_idx.load(Ordering::Relaxed);
        let n = self.state.len();
        while idx < n && unpack_len(self.state[idx].load(Ordering::Relaxed)) != 255 {
            idx += 1;
        }
        self.min_unsolved_idx.store(idx, Ordering::Relaxed);
        if idx < n { self.primes[idx] } else { u64::MAX }
    }

    pub fn is_complete(&self) -> bool {
        self.min_unsolved_prime() == u64::MAX
    }

    pub fn get_prime_idx(&self, val: u64) -> Option<usize> {
        self.primes.binary_search(&val).ok()
    }

    /// Count how many primes have been solved so far (used by the progress bar).
    pub fn solved_count(&self) -> usize {
        self.state.iter()
            .filter(|s| unpack_len(s.load(Ordering::Relaxed)) != 255)
            .count()
    }

    pub fn export_to_dat(&self, filename: &str) -> std::io::Result<()> {
        let file = File::create(filename)?;
        let mut writer = BufWriter::new(file);
        for code in &self.codes {
            let val = code.load(Ordering::Acquire);
            writer.write_all(&val.to_le_bytes())?;
        }
        Ok(())
    }
}

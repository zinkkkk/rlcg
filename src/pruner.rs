pub struct Pruner {
    fib: [u64; 93],
    pub target_max: u64,
}

impl Pruner {
    pub fn new(b1: u64) -> Self {
        let mut fib = [0u64; 93];
        fib[1] = 1;
        for i in 2..93 {
            fib[i] = fib[i - 1].saturating_add(fib[i - 2]);
        }
        Self { fib, target_max: b1 }
    }

    /// Combined upper-bound (B1) and lower-bound (Fibonacci reachability) check.
    /// Returns `true` if the current branch can still reach a valid target.
    pub fn is_viable(&self, c: u64, p: u64, steps_to_go: u8, target_min: u64) -> bool {
        // Upper bound: never exceed B1.
        if c > self.target_max { return false; }

        // Lower bound: can the Fibonacci-optimal growth from (c, p) in
        // `steps_to_go` more steps reach at least `target_min`?
        if steps_to_go >= 91 { return true; } // fib array only goes to index 92
        let st = steps_to_go as usize;

        let max_val = self.fib[st + 1]
            .saturating_mul(c)
            .saturating_add(self.fib[st].saturating_mul(p));

        max_val >= target_min
    }

    /// Beta-lookahead from the C code macro.
    ///
    /// Returns `true` when taking anything other than the maximal step is
    /// provably a dead end (i.e. even the second-best candidate `beta` cannot
    /// grow to reach `target_min` in the remaining steps).
    pub fn beta_lookahead_fails(
        &self,
        c: u64,
        p: u64,
        beta: u64,
        steps_to_go: u8,
        target_min: u64,
        max_c_flag: bool,
    ) -> bool {
        if steps_to_go == 0 { return false; }
        if steps_to_go >= 90 { return false; }
        let st = steps_to_go as usize;

        // a1: maximum reachable value if we take `beta` as the next step.
        // After the beta step we have (beta, c) and st-1 steps remain, giving
        // Fibonacci upper bound: fib[st]*beta + fib[st-1]*c.
        let a1 = self.fib[st].saturating_mul(beta)
            .saturating_add(self.fib[st - 1].saturating_mul(c));

        let mut upper_limit = a1;

        // a2: tighter specialised bound from the C code for the case where the
        // maximal candidate (c+p) was available but we chose not to take it.
        if st >= 3 && max_c_flag {
            let a2 = if st == 3 {
                c.saturating_mul(2).saturating_add(p.saturating_mul(3))
            } else if 2 * c <= 3 * p {
                self.fib[st].saturating_mul(c)
                    .saturating_add(self.fib[st + 1].saturating_mul(p))
            } else {
                let term1 = self.fib[st].saturating_add(2 * self.fib[st - 3]);
                let term2 = self.fib[st - 1].saturating_add(2 * self.fib[st - 4]);
                term1.saturating_mul(c).saturating_add(term2.saturating_mul(p))
            };

            if a2 > a1 {
                upper_limit = a2;
            }
        }

        upper_limit < target_min
    }

    /// Quick modular filter: returns `true` only for values coprime to 2·3·5·7·11·13.
    /// Used at the base case to skip values that cannot be prime targets.
    pub fn modular_filter(&self, val: u64) -> bool {
        !val.is_multiple_of(2)
            && !val.is_multiple_of(3)
            && !val.is_multiple_of(5)
            && !val.is_multiple_of(7)
            && !val.is_multiple_of(11)
            && !val.is_multiple_of(13)
    }

    /// GCD / divisibility prune.
    ///
    /// Returns `true` when the current chain state is provably unable to reach
    /// any odd prime target — the caller should **prune** (return early) when
    /// this returns `true`.
    ///
    /// Renamed from `gcd_prune` (which returned `true` to mean "prune") to
    /// make the polarity explicit at the call site:
    ///   `if pruner.should_gcd_prune(...) { return; }`
    pub fn should_gcd_prune(&self, values: &[u64]) -> bool {
        if values.len() < 3 { return false; }

        let c_val = values[0];
        if c_val.is_multiple_of(2) && values[1].is_multiple_of(2) {
            let mut kk = 1;
            while kk + 1 < values.len() && values[kk + 1].is_multiple_of(2) {
                kk += 1;
            }
            if kk >= 2 {
                let compare_val = values[kk + 1];
                if c_val <= 2 * compare_val {
                    return false;
                }
                for k in 0..kk {
                    let e = values[k + 1];
                    if (e + compare_val > c_val) && (e <= 2 * compare_val) {
                        return false;
                    }
                }
                return true; // Divisibility theorem applies — prune.
            }
        }
        false
    }
}

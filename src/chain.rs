#[derive(Clone, Copy, Debug)]
pub struct Step {
    pub value: u64,
    /// Index into `values` (newest-first) for the first  addend of this step.
    pub comp1: u8,
    /// Index into `values` (newest-first) for the second addend of this step.
    /// When comp1 == comp2 the step is a doubling.
    pub comp2: u8,
    pub dbl_count: u8,
}

/// Newest-first chain values backed by a fixed array with a front cursor.
///
/// DFS only ever grows/shrinks the chain at the front (the newest value), which
/// with a `Vec` meant `insert(0,..)`/`remove(0)` — an O(n) `memmove` on every
/// step that the profile showed costing ~13% of runtime. Here the values live
/// at the *back* of a fixed `[u64; CAP]` buffer and `head` marks the newest
/// element, so a push/pop is a single index move with no shifting and no
/// allocation. `CAP` matches `MAX_CHAIN_LEN`; chains never exceed it.
///
/// `Deref` yields the live `[u64]` slice in newest-first order, so every read
/// site (`find_candidates`, `should_gcd_prune`, indexing, iteration) is
/// unchanged — the `comp1`/`comp2` encoding indices keep their exact meaning.
const CAP: usize = 64;

#[derive(Clone)]
pub struct ChainVals {
    buf: [u64; CAP],
    head: usize,
}

impl ChainVals {
    #[inline]
    fn from_descending(vals: &[u64]) -> Self {
        let mut buf = [0u64; CAP];
        let head = CAP - vals.len();
        buf[head..].copy_from_slice(vals);
        Self { buf, head }
    }

    #[inline]
    pub fn push_front(&mut self, v: u64) {
        self.head -= 1;
        self.buf[self.head] = v;
    }

    #[inline]
    pub fn pop_front(&mut self) {
        self.head += 1;
    }
}

impl std::ops::Deref for ChainVals {
    type Target = [u64];
    #[inline]
    fn deref(&self) -> &[u64] { &self.buf[self.head..] }
}

pub struct LucasChain {
    pub values: ChainVals,
    pub steps: Vec<Step>,
}

impl LucasChain {
    /// Create a chain seeded with the standard Lucas-chain prefix [1, 2, 3].
    ///
    /// # Bootstrap step index semantics
    ///
    /// `comp1` / `comp2` in each `Step` record the positions in `values`
    /// **at the moment the step was taken**.  Because `values` is stored
    /// newest-first and new values are prepended, the indices in historical
    /// steps become stale once newer values are added — e.g. after extending
    /// the chain, the index `0` in an old step no longer refers to the same
    /// element it did when that step was recorded.
    ///
    /// This is *intentional and harmless* for the three bootstrap steps below
    /// because `encode()` explicitly skips the first `lower_limit` (4 or 5)
    /// steps when serialising, so the stale indices in steps[0..=2] are never
    /// re-read by the encoder.  The DFS candidate generator reads only
    /// `steps.last()`, so it also never touches these bootstrap records.
    ///
    /// The indices stored here are therefore placeholders that preserve the
    /// correct `dbl_count` progression (0 → 1 → 1) without needing to be
    /// geometrically valid.
    pub fn new() -> Self {
        Self {
            values: ChainVals::from_descending(&[3, 2, 1]),
            steps: vec![
                // step 0: value=1  (seed — no real predecessor)
                Step { value: 1, comp1: 0, comp2: 0, dbl_count: 0 },
                // step 1: value=2 = 1+1  (doubling of 1)
                Step { value: 2, comp1: 0, comp2: 0, dbl_count: 1 },
                // step 2: value=3 = 2+1  (addition; indices stale by design)
                Step { value: 3, comp1: 0, comp2: 1, dbl_count: 1 },
            ],
        }
    }

    pub fn last_val(&self) -> u64 { self.values[0] }
    pub fn prev_val(&self) -> u64 { self.values[1] }

    /// Encode the chain into a compact u64 bit-string.
    ///
    /// This is a direct translation of the C macro `ENCODE_LCHAIN_TEMPLATE`.
    ///
    /// # Bootstrap selector (`lower_limit`)
    ///
    /// The encoder skips the first few steps — the "bootstrap prefix" — and
    /// instead records which prefix was used in the low 3 bits of the output
    /// word (the `start_code`).  The decodable region starts above
    /// `lower_limit`:
    ///
    /// * `lower_limit = 5`: prefix is one of the standard 5-step starts
    ///   (chain values up through index 4 are implicit).
    /// * `lower_limit = 6`: prefix extends to 6 steps because `steps[4].value
    ///   == 8`, which only occurs for the `{1,2,3,5,8,...}` family; the 6th
    ///   value is then the first encoded step.
    ///
    /// # Step-type encoding table
    ///
    /// Each step above `lower_limit` is classified by its `(comp1, comp2)`
    /// pair and assigned a type 1–8:
    ///
    /// | type | (comp1, comp2)       | description                       |
    /// |------|----------------------|-----------------------------------|
    /// |  1   | (0, 1)               | maximal addition: top + prev      |
    /// |  2   | (0, 2)               | addition:  top + values[2]        |
    /// |  3   | (1, 1)               | doubling of prev                  |
    /// |  4   | (2, 2)               | doubling of values[2]             |
    /// |  5   | (0, c), c > 2        | addition:  top + values[c]        |
    /// |  6   | (1, c), c > 1        | addition:  prev + values[c]       |
    /// |  7   | (2, 3)               | addition:  values[2] + values[3]  |
    /// |  8   | (2, 4)               | addition:  values[2] + values[4]  |
    ///
    /// Types 2 and 3 bundle a run of `j` trailing type-1 (maximal) steps
    /// using a compact variable-length encoding to reduce bit usage for long
    /// Fibonacci-like tails.
    pub fn encode(&self) -> Option<u64> {
        let mut chain_code: u64 = 0;
        let mut code_len: u32 = 0;
        let mut len = self.steps.len();

        // Determine how many leading steps form the implicit bootstrap prefix.
        // steps[4].value == 8  →  the {1,2,3,5,8} family; prefix is 6 steps deep.
        // Everything else      →  standard 5-step prefix.
        let lower_limit = if len >= 5 && self.steps[4].value == 8 { 6 } else { 5 };

        if len > lower_limit {
            let mut index1 = self.steps[len - 1].comp1;
            let mut index2 = self.steps[len - 1].comp2;

            // Strip any trailing type-1 (maximal, comp1=0 comp2=1) steps from
            // the *top* of the step list; the decoder re-derives them implicitly.
            if index1 == 0 && index2 == 1 {
                while len > lower_limit && index1 == 0 && index2 == 1 {
                    len -= 1;
                    if len > 0 {
                        index1 = self.steps[len - 1].comp1;
                        index2 = self.steps[len - 1].comp2;
                    }
                }
            }

            if len > lower_limit {
                let mut step_type  = Vec::new();
                let mut step_index2 = Vec::new();

                while len > lower_limit {
                    let st = match (index1, index2) {
                        (0, 1)          => 1,
                        (0, 2)          => 2,
                        (1, 1)          => 3,
                        (2, 2)          => 4,
                        (0, c) if c > 2 => 5,
                        (1, c) if c > 1 => 6,
                        (2, 3)          => 7,
                        (2, 4)          => 8,
                        _               => 1,
                    };
                    step_type.push(st);
                    step_index2.push(index2);

                    len -= 1;
                    if len > 0 {
                        index1 = self.steps[len - 1].comp1;
                        index2 = self.steps[len - 1].comp2;
                    }
                }

                let mut type_index = 0;
                let type_count = step_type.len();

                while type_index < type_count {
                    code_len += 4;
                    if code_len > 64 { return None; }
                    chain_code = chain_code.wrapping_shl(4);

                    let mut increment = 1;

                    match step_type[type_index] {
                        // Type 1 (maximal step): encoded as 0x0 — nothing extra needed.
                        1 => {}

                        // Types 2 and 3 bundle `j` following type-1 steps.
                        // `j` is split into i_part*12 + j_part for compact encoding.
                        2 | 3 => {
                            let mut j = 0;
                            while type_index + j + 1 < type_count
                                && step_type[type_index + j + 1] == 1
                            {
                                j += 1;
                            }
                            increment = j + 1;

                            let mut i_part = 0usize;
                            let mut j_part = j;
                            while j_part >= 12 {
                                j_part -= 12;
                                i_part += 1;
                            }

                            let base: u64 = if step_type[type_index] == 2 { 0x1 } else { 0x2 };
                            match j_part {
                                0 => chain_code = chain_code.wrapping_add(base),
                                1 => chain_code = chain_code.wrapping_add(base + 2),
                                2 => chain_code = chain_code.wrapping_add(base + 4),
                                3 => chain_code = chain_code.wrapping_add(base + 6),
                                4..=7 => {
                                    code_len += 2;
                                    if code_len > 64 { return None; }
                                    chain_code = chain_code.wrapping_shl(2);
                                    let offset = if base == 1 { 0x9 } else { 0xA };
                                    chain_code = chain_code
                                        .wrapping_add(((j_part as u64 - 4) << 4) + offset);
                                }
                                8..=11 => {
                                    code_len += 2;
                                    if code_len > 64 { return None; }
                                    chain_code = chain_code.wrapping_shl(2);
                                    let offset = if base == 1 { 0xB } else { 0xC };
                                    chain_code = chain_code
                                        .wrapping_add(((j_part as u64 - 8) << 4) + offset);
                                }
                                _ => {}
                            }

                            // i_part encodes multiples of 12 extra maximal steps.
                            // i_part == 0 needs no extra bits.
                            match i_part {
                                0 => {}
                                1 => {
                                    code_len += 6;
                                    if code_len > 64 { return None; }
                                    chain_code = chain_code.wrapping_shl(6);
                                }
                                2 => {
                                    code_len += 6;
                                    if code_len > 64 { return None; }
                                    chain_code = chain_code.wrapping_shl(6).wrapping_add(0x10);
                                }
                                3 => {
                                    code_len += 6;
                                    if code_len > 64 { return None; }
                                    chain_code = chain_code.wrapping_shl(6).wrapping_add(0x20);
                                }
                                _ => {}
                            }
                        }

                        // Type 4: doubling of values[2]; 6-bit opcode 0x30.
                        4 => {
                            code_len += 2;
                            if code_len > 64 { return None; }
                            chain_code = chain_code.wrapping_shl(2).wrapping_add(0x30);
                        }

                        // Type 5: top + values[c], c > 2; 6-bit opcode.
                        5 => {
                            let idx = step_index2[type_index].saturating_sub(3);
                            code_len += 2;
                            if code_len > 64 { return None; }
                            chain_code = chain_code.wrapping_shl(2);
                            match idx {
                                0..=3 => chain_code = chain_code.wrapping_add(idx as u64 * 16 + 0xE),
                                4     => chain_code = chain_code.wrapping_add(0x0D),
                                5     => chain_code = chain_code.wrapping_add(0x1D),
                                // No opcode exists for index2 > 8 (idx > 5). The C
                                // original returns 0 here; we return None so the
                                // search records a different, encodable chain
                                // instead of emitting corrupt bits.
                                _     => return None,
                            }
                        }

                        // Type 6: prev + values[c], c > 1; 6-bit opcode.
                        6 => {
                            let idx = step_index2[type_index].saturating_sub(2);
                            // Only idx 0..=3 fit the 6-bit opcode (idx is packed
                            // into bits[5:4]). For index2 > 5 the C original
                            // returns 0; we return None so a different, encodable
                            // chain is recorded instead of corrupting the stream
                            // (idx*16 would overflow past bit 5).
                            if idx > 3 { return None; }
                            code_len += 2;
                            if code_len > 64 { return None; }
                            chain_code = chain_code
                                .wrapping_shl(2)
                                .wrapping_add(idx as u64 * 16 + 0xF);
                        }

                        // Type 7: values[2] + values[3]; 6-bit opcode 0x2D.
                        7 => {
                            code_len += 2;
                            if code_len > 64 { return None; }
                            chain_code = chain_code.wrapping_shl(2).wrapping_add(0x2D);
                        }

                        // Type 8: values[2] + values[4]; 6-bit opcode 0x3D.
                        8 => {
                            code_len += 2;
                            if code_len > 64 { return None; }
                            chain_code = chain_code.wrapping_shl(2).wrapping_add(0x3D);
                        }

                        _ => {}
                    }
                    type_index += increment;
                }
            }
        }

        // Append the 3-bit bootstrap selector in the lowest bits.
        code_len += 3;
        if code_len > 64 { return None; }
        chain_code = chain_code.wrapping_shl(3);

        // Bootstrap selector: encodes which of the 8 standard starting
        // sequences was used, identified by steps[3], steps[4], (steps[5]).
        //
        // start_code  steps[3]  steps[4]  steps[5]   sequence
        //     0x0        4         6         —        {1,2,3,4,6}
        //     0x1        4         5         —        {1,2,3,4,5}
        //     0x2        4         7         —        {1,2,3,4,7}
        //     0x3        5         6         —        {1,2,3,5,6}
        //     0x4        5         7         —        {1,2,3,5,7}
        //     0x5        5         8        10        {1,2,3,5,8,10}
        //     0x6        5         8        11        {1,2,3,5,8,11}
        //     0x7        5         8        13        {1,2,3,5,8,13}
        let val3 = if self.steps.len() > 3 { self.steps[3].value } else { 0 };
        let val4 = if self.steps.len() > 4 { self.steps[4].value } else { 0 };
        let val5 = if self.steps.len() > 5 { self.steps[5].value } else { 0 };

        let start_code: u64 = match val3 {
            4 => match val4 {
                5 => 0x1,
                6 => 0x0,
                7 => 0x2,
                _ => 0,
            },
            5 => match val4 {
                6 => 0x3,
                7 => 0x4,
                8 => match val5 {
                    10 => 0x5,
                    11 => 0x6,
                    _  => 0x7, // 13 and any other fallback
                },
                _ => 0,
            },
            _ => 0,
        };

        Some(chain_code.wrapping_add(start_code))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Build a chain whose recorded steps are exactly `tail`, layered on top of a
    // 5-step bootstrap prefix (so lower_limit == 5 and the tail is encoded).
    // encode() reads only `steps`, never `values`, so the values are irrelevant
    // to what these tests exercise.
    fn chain_with_tail(tail: &[Step]) -> LucasChain {
        let mut c = LucasChain::new();
        // steps[3], steps[4]: pick a non-{1,2,3,5,8} family so lower_limit == 5.
        c.steps.push(Step { value: 5, comp1: 0, comp2: 1, dbl_count: 1 }); // val3=5
        c.steps.push(Step { value: 7, comp1: 0, comp2: 1, dbl_count: 1 }); // val4=7
        c.steps.extend_from_slice(tail);
        c
    }

    fn t1() -> Step { Step { value: 0, comp1: 0, comp2: 1, dbl_count: 1 } } // maximal

    // Regression: a type-5 step (top + values[c]) with c > 8 has no opcode.
    // The C original returns 0; encode() must return None rather than silently
    // emitting the c == 3 opcode (0xE), which corrupted the bit stream and made
    // the file fail validation.
    #[test]
    fn deep_type5_returns_none() {
        let deep = Step { value: 0, comp1: 0, comp2: 9, dbl_count: 1 }; // idx = 6
        let chain = chain_with_tail(&[t1(), t1(), deep]);
        assert_eq!(chain.encode(), None);
    }

    // Regression: a type-6 step (prev + values[c]) with c > 5 has no opcode and
    // would overflow idx*16 past bit 5, corrupting the opcode and every later
    // step. encode() must return None (C returns 0).
    #[test]
    fn deep_type6_returns_none() {
        let deep = Step { value: 0, comp1: 1, comp2: 6, dbl_count: 1 }; // idx = 4
        let chain = chain_with_tail(&[t1(), t1(), deep]);
        assert_eq!(chain.encode(), None);
    }

    // Boundary cases that DO have opcodes must still encode successfully.
    #[test]
    fn boundary_indices_encode_some() {
        // type-5 c == 8 (idx 5, opcode 0x1D) and type-6 c == 5 (idx 3) are the
        // deepest representable references.
        let t5 = Step { value: 0, comp1: 0, comp2: 8, dbl_count: 1 };
        assert!(chain_with_tail(&[t1(), t1(), t5]).encode().is_some());
        let t6 = Step { value: 0, comp1: 1, comp2: 5, dbl_count: 1 };
        assert!(chain_with_tail(&[t1(), t1(), t6]).encode().is_some());
    }
}

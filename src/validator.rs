use std::fs::File;
use std::io::Read;

/// Validate an existing file on its own terms: the target primes are derived
/// from the file's code count (N codes → first N primes from 11), so no B1 is
/// needed. Reports the chain count and whether the file is valid.
pub fn validate_file_standalone(filename: &str, make_targets: impl Fn(usize) -> Vec<u64>) -> bool {
    let len = match std::fs::metadata(filename) {
        Ok(m) => m.len() as usize,
        Err(e) => {
            println!("Could not open {filename}: {e}");
            return false;
        }
    };
    if len % 8 != 0 {
        println!("{filename} is {len} bytes, not a multiple of 8 — not a valid chain file.");
        return false;
    }
    let count = len / 8;
    let targets = make_targets(count);
    validate_file(filename, &targets)
}

pub fn validate_file(filename: &str, target_primes: &[u64]) -> bool {

    println!("\nStarting Validator on {}...", filename);
    let mut file = match File::open(filename) {
        Ok(f) => f,
        Err(e) => {
            println!("Could not open {}: {}", filename, e);
            return false;
        }
    };

    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer).unwrap();

    let codes_count = buffer.len() / 8;
    println!("Found {} encoded chains.", codes_count);

    if codes_count != target_primes.len() {
        println!(
            "WARNING: File code count ({}) does not match Target Primes count ({})",
            codes_count,
            target_primes.len()
        );
    }

    let mut success_count = 0;
    let limit = std::cmp::min(codes_count, target_primes.len());

    for i in 0..limit {
        let code =
            u64::from_le_bytes(buffer[i * 8..(i + 1) * 8].try_into().unwrap());
        let target = target_primes[i];
        if let Err(msg) = decode_and_check(code, target) {
            println!("VALIDATION FAILED at index {i} (prime {target}): {msg}");
            return false;
        }
        success_count += 1;
    }

    println!("SUCCESS! {}/{} chains mathematically verified.", success_count, limit);
    true
}

/// Decode a single packed chain `code` and verify it reaches `target` via valid
/// Lucas-chain steps. Returns `Err(message)` instead of panicking so a corrupt
/// file is reported cleanly (important under `panic = "abort"`).
fn decode_and_check(mut code: u64, target: u64) -> Result<(), String> {
    {
        let mut chain = vec![1u64, 2, 3];
        let start_seq = code & 7;
        code >>= 3; // strip the 3-bit bootstrap selector

        // Bootstrap sequence: these values are "known" and not re-decoded.
        // start_seq 5/6/7 add a 6th element; the chain module treats that 6th element
        // as the first *encoded* step (lower_limit=5, not 6), so the decoder still needs
        // to reconstruct it from the opcode stream — the bootstrap here is intentionally
        // only 5 deep for those variants.
        match start_seq {
            0 => { chain.push(4); chain.push(6); }
            1 => { chain.push(4); chain.push(5); }
            2 => { chain.push(4); chain.push(7); }
            3 => { chain.push(5); chain.push(6); }
            4 => { chain.push(5); chain.push(7); }
            5 => { chain.push(5); chain.push(8); chain.push(10); }
            6 => { chain.push(5); chain.push(8); chain.push(11); }
            7 => { chain.push(5); chain.push(8); chain.push(13); }
            _ => unreachable!(),
        }

        while code > 0 {
            let b6 = code & 0x3F;
            let b4 = code & 0xF;

            // ---------------------------------------------------------------
            // 'i' bundle: encodes 12, 24, or 36 consecutive maximal steps.
            // The encoder stores (i_part - 1) in bits[5:4], so the count is
            // always at least 12. We must add 1 before multiplying.
            //
            // BUG FIX #2: was `((code >> 4) & 3) * 12`, which decoded
            // i_part=1 (the most common case) as 0 maximal steps instead of 12.
            // ---------------------------------------------------------------
            if b4 == 0x0 && b6 != 0x30 {
                let max_adds = (((code >> 4) & 3) + 1) * 12;
                for _ in 0..max_adds {
                    let len = chain.len();
                    chain.push(chain[len - 1] + chain[len - 2]);
                }
                code >>= 6;
                continue;
            }

            // ---------------------------------------------------------------
            // process_step: apply `max_adds` maximal steps *before* the
            // special step.
            //
            // BUG FIX #1: the encoder bundles j chronologically-earlier maximal
            // steps together with the special step.  In the bit stream the
            // maximal-step count is packed at a higher bit position than the
            // opcode, so it is decoded *after* the opcode — but chronologically
            // those maximal steps precede the special step.
            // Old code did special_step first then maximal steps: WRONG.
            // ---------------------------------------------------------------
            let process_step = |chain: &mut Vec<u64>, c1_off: usize, c2_off: usize, max_adds: u64|
                -> Result<(), String> {
                // First: the j maximal (consecutive) steps that come before this step.
                for _ in 0..max_adds {
                    let len = chain.len();
                    chain.push(chain[len - 1] + chain[len - 2]);
                }
                // Then: the special (non-maximal) step itself.
                let len = chain.len();
                let a = chain[len - 1 - c1_off];
                let b = chain[len - 1 - c2_off];

                // Sanity check: the difference must be in the chain (or zero for doublings).
                let diff = a.abs_diff(b);
                if a != b && !chain.contains(&diff) {
                    return Err(format!(
                        "difference {diff} not in chain (a={a}, b={b}, chain={chain:?})"
                    ));
                }

                chain.push(a + b);
                Ok(())
            };

            // Opcode dispatch — 4-bit opcodes (types 1–8):
            if      b4 == 0x1 { process_step(&mut chain, 0, 2, 0)?; code >>= 4; }
            else if b4 == 0x2 { process_step(&mut chain, 1, 1, 0)?; code >>= 4; }
            else if b4 == 0x3 { process_step(&mut chain, 0, 2, 1)?; code >>= 4; }
            else if b4 == 0x4 { process_step(&mut chain, 1, 1, 1)?; code >>= 4; }
            else if b4 == 0x5 { process_step(&mut chain, 0, 2, 2)?; code >>= 4; }
            else if b4 == 0x6 { process_step(&mut chain, 1, 1, 2)?; code >>= 4; }
            else if b4 == 0x7 { process_step(&mut chain, 0, 2, 3)?; code >>= 4; }
            else if b4 == 0x8 { process_step(&mut chain, 1, 1, 3)?; code >>= 4; }
            // 6-bit opcodes:
            else if b6 == 0x30 { process_step(&mut chain, 2, 2, 0)?; code >>= 6; }  // type 4
            else if b4 == 0x9 { process_step(&mut chain, 0, 2, 4 + ((code >> 4) & 3))?; code >>= 6; }
            else if b4 == 0xA { process_step(&mut chain, 1, 1, 4 + ((code >> 4) & 3))?; code >>= 6; }
            else if b4 == 0xB { process_step(&mut chain, 0, 2, 8 + ((code >> 4) & 3))?; code >>= 6; }
            else if b4 == 0xC { process_step(&mut chain, 1, 1, 8 + ((code >> 4) & 3))?; code >>= 6; }
            else if b4 == 0xE { process_step(&mut chain, 0, 3 + ((code >> 4) & 3) as usize, 0)?; code >>= 6; }
            else if b4 == 0xF { process_step(&mut chain, 1, 2 + ((code >> 4) & 3) as usize, 0)?; code >>= 6; }
            else if b6 == 0x0D { process_step(&mut chain, 0, 7, 0)?; code >>= 6; }
            else if b6 == 0x1D { process_step(&mut chain, 0, 8, 0)?; code >>= 6; }
            else if b6 == 0x2D { process_step(&mut chain, 2, 3, 0)?; code >>= 6; }
            else if b6 == 0x3D { process_step(&mut chain, 2, 4, 0)?; code >>= 6; }
            else {
                return Err(format!("unknown opcode 0x{b6:02X}"));
            }
        }

        // Implicit trailing maximal steps: the encoder strips these from the bit
        // stream because they can be re-derived trivially.
        while *chain.last().unwrap() < target {
            let len = chain.len();
            chain.push(chain[len - 1] + chain[len - 2]);
        }

        if *chain.last().unwrap() != target {
            return Err(format!("chain ended at {}, expected {target}", chain.last().unwrap()));
        }
    }
    Ok(())
}
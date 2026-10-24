# rlcg — Lucas Chain Generator

`rlcg` generates a file of **Lucas chain codes** for every prime up to a chosen
bound *B1*, for use as a precomputed addition-chain table in
[GMP-ECM](https://gitlab.inria.fr/zimmermann/ecm)'s stage-1 scalar
multiplication (PRAC / Montgomery ladders).

For each prime `p` in `(11, B1]` it searches for a short Lucas chain reaching
`p`, encodes that chain into a compact 64-bit word, and writes all words, in
prime order, to `Lchain_codes.dat`.

## Building

```sh
cargo build --release
```

The optimized binary is at `target/release/rlcg`.

## Usage

**Generate** (writes `Lchain_codes.dat`, then always validates it):

```sh
rlcg -b <B1> [-t <threads>]
```

**Validate only** an existing file (no generation, no `B1` needed):

```sh
rlcg -i
```

| Flag | Meaning |
|------|---------|
| `-b <B1>` | Smoothness bound. Every prime up to `B1` gets a chain. Must be ≥ 13. Required for generation; not used with `-i`. |
| `-t <N>`  | Worker thread count. Defaults to the number of logical CPUs. |
| `-i`      | Validate the existing `Lchain_codes.dat` and exit. The number of chains and the target primes are read from the file itself, so no `-b` is needed. Exits non-zero if the file is missing or invalid. |

Generation **always** validates the file it just wrote and prints the result;
there is no separate "check" flag. A validation failure exits non-zero.

### Examples

```sh
# Generate chains for all primes up to 1,000,000 (validated automatically)
rlcg -b 1000000

# Validate an existing Lchain_codes.dat
rlcg -i

# Limit to 8 worker threads
rlcg -b 1000000 -t 8
```

## Output

A single file named **`Lchain_codes.dat`** is written in the current directory.
The name is fixed because GMP-ECM expects exactly this filename. The file is
raw little-endian `u64` words — one 8-byte code per target prime, in ascending
prime order. It contains no header.

## Notes on performance

- The search is fully parallel and scales with available cores. Work
  granularity (how the search tree is split across threads) adapts to the
  thread count automatically, so machines with many cores stay well-utilized.
- Runtime grows quickly with `B1`, since the number of primes and the depth of
  the chains both increase. Small bounds (up to a few hundred thousand)
  complete in well under a second; large bounds (millions) take seconds to
  tens of seconds.
- Output is **not bit-for-bit deterministic** across runs: when more than one
  shortest chain exists for a prime, parallel workers may record different but
  equally-good chains. Every chain written is always a valid shortest (or
  equal-length) chain for its prime — `-c` verifies this.

## Verifying output

Validation runs automatically at the end of every generation, and on demand via
`-i`. The validator independently decodes every code and confirms it forms a
valid Lucas chain ending exactly at the corresponding prime. It prints the
number of chains checked; a failure is reported with the offending prime and the
reason, and the program exits with a non-zero status.

With `-i`, the chain count comes straight from the file size (one 8-byte code
per chain) and the target primes are reconstructed from that count — so you can
validate any `Lchain_codes.dat` without remembering which `B1` produced it.

## Verifying output

Yes, this is vibe coded.

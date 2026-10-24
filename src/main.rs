use indicatif::{ProgressBar, ProgressStyle};
use std::sync::Arc;

mod candidates;
mod chain;
mod pruner;
mod registry;
mod search_engine;
mod validator;

/// Output filename. Fixed because GMP-ECM expects this exact name.
const OUTPUT_FILE: &str = "Lchain_codes.dat";

/// Generate all prime targets in (11, b1].
/// These are the values that each get a Lucas chain.
fn generate_targets(limit: u64) -> Vec<u64> {
    println!("Generating prime targets up to {}...", limit);
    let sieve = primal::Sieve::new(limit as usize);
    let primes: Vec<u64> = sieve
        .primes_from(11)
        .take_while(|&p| p as u64 <= limit)
        .map(|p| p as u64)
        .collect();
    println!("Found {} target primes.", primes.len());
    primes
}

/// The first `n` prime targets (the primes from 11 onward, in order). Used by
/// `-i` to reconstruct the target list from an existing file's code count
/// without needing a B1 value.
fn first_n_primes(n: usize) -> Vec<u64> {
    if n == 0 {
        return Vec::new();
    }
    // The n-th prime from 11 is below the n-th prime overall; estimate an upper
    // bound generously, then take exactly n.
    let (_, hi) = primal::estimate_nth_prime((n + 6) as u64); // +6 skips 2,3,5,7 and pads
    let sieve = primal::Sieve::new(hi as usize);
    sieve.primes_from(11).map(|p| p as u64).take(n).collect()
}

const HELP: &str = "\
Optimized Lucas Chain Generator

USAGE:
    rlcg -b <B1> [-t <THREADS>]   Generate Lchain_codes.dat (then validate it)
    rlcg -i                       Validate an existing Lchain_codes.dat

OPTIONS:
    -b <B1>        Smoothness bound; every prime up to B1 gets a chain (>= 13).
    -t <THREADS>   Worker thread count (default: logical CPU count).
    -i             Validate the existing Lchain_codes.dat and exit (no B1 needed).
    -h, --help     Print this help.
    -V, --version  Print version.";

#[derive(Debug, Default)]
struct Args {
    b1: Option<u64>,
    threads: Option<usize>,
    check_input: bool,
}

/// Minimal hand-rolled arg parser. Supports `-b N`/`-bN`, `-t N`/`-tN`, `-i`,
/// `-h`/`--help`, `-V`/`--version`. Exits with a usage error on anything else.
fn parse_args() -> Args {
    let mut args = Args::default();
    let mut it = std::env::args().skip(1);

    // Parse the numeric value for a flag, whether glued (`-b13`) or separate
    // (`-b 13`). `glued` is the remainder after the flag char, if any.
    fn value<T: std::str::FromStr>(
        flag: char,
        glued: &str,
        it: &mut impl Iterator<Item = String>,
    ) -> T {
        let raw = if !glued.is_empty() {
            glued.to_string()
        } else {
            it.next()
                .unwrap_or_else(|| usage_error(&format!("-{flag} requires a value")))
        };
        raw.parse()
            .unwrap_or_else(|_| usage_error(&format!("invalid value for -{flag}: '{raw}' max -b value is 18446744073709551615")))
    }

    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{HELP}");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("rlcg {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "-i" => args.check_input = true,
            s if s.starts_with("-b") => args.b1 = Some(value('b', &s[2..], &mut it)),
            s if s.starts_with("-t") => args.threads = Some(value('t', &s[2..], &mut it)),
            other => usage_error(&format!("unexpected argument '{other}'")),
        }
    }
    args
}

fn usage_error(msg: &str) -> ! {
    eprintln!("error: {msg}\n\n{HELP}");
    std::process::exit(2);
}

fn main() {
    let args = parse_args();

    // -i: validate the existing file and stop. Targets are derived from the
    // file's own code count, so no B1 is required.
    if args.check_input {
        let valid = validator::validate_file_standalone(OUTPUT_FILE, first_n_primes);
        std::process::exit(if valid { 0 } else { 1 });
    }

    let b1_limit = match args.b1 {
        Some(b) if b >= 13 => b,
        Some(_) => {
            eprintln!("B1 must be at least 13.");
            std::process::exit(1);
        }
        None => {
            eprintln!(
                "B1 is required for generation. Use -b <B1>, or -i to validate an existing file."
            );
            std::process::exit(1);
        }
    };

    let num_threads = match args.threads {
        None => rayon::ThreadPoolBuilder::new()
            .build()
            .unwrap()
            .current_num_threads(),
        Some(tc) => rayon::ThreadPoolBuilder::new()
            .num_threads(tc)
            .build()
            .unwrap()
            .current_num_threads(),
    };

    println!("Using {} worker threads.", num_threads);

    let targets = generate_targets(b1_limit);

    let total_primes = targets.len() as u64;
    let registry = Arc::new(registry::ChainRegistry::new(targets.clone()));
    registry.bootstrap_small_primes();

    let pruner = Arc::new(pruner::Pruner::new(b1_limit));

    // Progress bar — updated by a lightweight ticker thread so it stays live
    // without any calls inside the hot DFS path.
    let pb = ProgressBar::new(total_primes);
    pb.set_style(
        ProgressStyle::default_bar()
            .template(
                "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] \
                 {pos}/{len} primes solved",
            )
            .unwrap(),
    );

    // Ticker: update the progress bar every 250 ms from a background thread
    // so the bar stays smooth without adding any overhead to the DFS workers.
    let registry_tick = Arc::clone(&registry);
    let pb_tick = pb.clone();
    let ticker = std::thread::spawn(move || {
        loop {
            let solved = registry_tick.solved_count() as u64;
            pb_tick.set_position(solved);
            if solved >= total_primes {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    });

    // Single call — the work channel feeds all lengths continuously with no
    // barrier between passes.
    search_engine::run_search_all_lengths(Arc::clone(&registry), Arc::clone(&pruner), num_threads);

    let _ = ticker.join();
    pb.set_position(registry.solved_count() as u64);
    pb.finish_with_message("done");

    registry.export_to_dat(OUTPUT_FILE).unwrap_or_else(|e| {
        eprintln!("Failed to write {OUTPUT_FILE}: {e}");
        std::process::exit(1);
    });
    println!("Saved {OUTPUT_FILE}");

    // Always validate the freshly written file and report the result.
    if !validator::validate_file(OUTPUT_FILE, &targets) {
        eprintln!("Output validation FAILED.");
        std::process::exit(1);
    }
}

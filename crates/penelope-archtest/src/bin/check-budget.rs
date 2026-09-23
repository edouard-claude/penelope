//! `check-budget` : compare deux `budget.toml` (base, HEAD) et imprime ce qui remonte.
//!
//! Appelé par `scripts/check-budget.sh`, qui fait la partie git (base, renommages,
//! trailer de dérogation). Usage :
//!
//! ```text
//! check-budget <base.toml> <head.toml> [renames.tsv] [étiquette de la base]
//! ```
//!
//! `renames.tsv` : une ligne `ancien<TAB>nouveau` par fichier renommé. Sortie 1 si au
//! moins une régression, 2 si les fichiers sont illisibles.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(base_path), Some(head_path)) = (args.first(), args.get(1)) else {
        eprintln!("usage : check-budget <base.toml> <head.toml> [renames.tsv] [base]");
        return ExitCode::from(2);
    };
    let label = args
        .get(3)
        .cloned()
        .unwrap_or_else(|| "la base".to_string());
    let read = |p: &str| std::fs::read_to_string(p).map_err(|e| format!("{p} : {e}"));
    let renames: BTreeMap<String, String> = match args.get(2).map(|p| read(p)) {
        None => BTreeMap::new(),
        Some(Ok(raw)) => raw
            .lines()
            .filter_map(|l| l.split_once('\t'))
            .map(|(old, new)| (old.trim().to_string(), new.trim().to_string()))
            .collect(),
        Some(Err(e)) => {
            eprintln!("check-budget : {e}");
            return ExitCode::from(2);
        }
    };
    let (base, head) = match (read(base_path), read(head_path)) {
        (Ok(b), Ok(h)) => (b, h),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("check-budget : {e}");
            return ExitCode::from(2);
        }
    };
    match penelope_archtest::ratchet::regressions(&base, &head, &renames, &label) {
        Ok(r) if r.is_empty() => ExitCode::SUCCESS,
        Ok(r) => {
            for line in r {
                println!("{line}");
            }
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("check-budget : {e}");
            ExitCode::from(2)
        }
    }
}

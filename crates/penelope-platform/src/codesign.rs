//! Signature de code macOS (issue #28).
//!
//! Un binaire compilé par `cargo build` ne porte qu'une signature ad hoc, dont l'exigence
//! désignée (`cdhash H"…"`) change à chaque build : les autorisations du Trousseau sont
//! redemandées. Signé avec une identité stable et un identifiant fixe, l'exigence devient
//! `identifier "…" and certificate …` et survit aux builds.

use std::path::Path;
use std::process::Command;

/// Identifiant stable du binaire signé.
pub const DEFAULT_IDENTIFIER: &str = "io.github.edouard-claude.penelope";

const CODESIGN: &str = "/usr/bin/codesign";

/// Type de signature d'un binaire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signature {
    /// Plateforme sans signature de code.
    Unsupported,
    Unsigned,
    AdHoc {
        identifier: String,
    },
    Identity {
        identifier: String,
        authority: String,
    },
}

impl Signature {
    pub fn describe(&self) -> String {
        match self {
            Signature::Unsupported => "sans objet hors macOS".into(),
            Signature::Unsigned => "binaire non signé".into(),
            Signature::AdHoc { identifier } => format!("signature ad hoc ({identifier})"),
            Signature::Identity {
                identifier,
                authority,
            } => format!("signé par « {authority} » ({identifier})"),
        }
    }
}

/// Lit la sortie de `codesign -dvv`.
pub fn parse_details(output: &str) -> Signature {
    if output.contains("not signed at all") {
        return Signature::Unsigned;
    }
    let field = |key: &str| {
        output
            .lines()
            .find_map(|l| l.strip_prefix(key))
            .map(|v| v.trim().to_string())
    };
    let identifier = field("Identifier=").unwrap_or_default();
    match field("Authority=") {
        Some(authority) if field("Signature=").as_deref() != Some("adhoc") => Signature::Identity {
            identifier,
            authority,
        },
        _ => Signature::AdHoc { identifier },
    }
}

/// Signature du binaire `path`.
pub fn inspect(path: &Path) -> Signature {
    if !cfg!(target_os = "macos") {
        return Signature::Unsupported;
    }
    match Command::new(CODESIGN).arg("-dvv").arg(path).output() {
        Ok(out) => parse_details(&format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )),
        Err(_) => Signature::Unsupported,
    }
}

/// Exigence désignée du binaire (`codesign -d -r-`) ; implicite (`# designated => cdhash …`)
/// pour une signature ad hoc.
pub fn designated_requirement(path: &Path) -> Option<String> {
    let out = Command::new(CODESIGN)
        .args(["-d", "-r-"])
        .arg(path)
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.trim_start_matches("# ").strip_prefix("designated => "))
        .map(str::to_string)
}

/// Signe `path` avec `identity` et un identifiant fixe, puis vérifie la signature.
pub fn sign(path: &Path, identity: &str, identifier: &str) -> Result<(), String> {
    if !cfg!(target_os = "macos") {
        return Ok(());
    }
    let run = |args: &[&str]| -> Result<(), String> {
        let out = Command::new(CODESIGN)
            .args(args)
            .arg(path)
            .output()
            .map_err(|e| format!("codesign : {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "codesign {} : {}",
                args.first().copied().unwrap_or_default(),
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    };
    run(&[
        "--force",
        "--timestamp=none",
        "--sign",
        identity,
        "--identifier",
        identifier,
    ])?;
    run(&["--verify", "--strict"])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codesign_details_are_classified() {
        let adhoc = "Executable=/x/penelope\nIdentifier=penelope-d7f5146ec7d0636c\n\
                     CodeDirectory v=20400 flags=0x20002(adhoc,linker-signed)\nSignature=adhoc\n";
        assert_eq!(
            parse_details(adhoc),
            Signature::AdHoc {
                identifier: "penelope-d7f5146ec7d0636c".into()
            }
        );
        let signed = "Identifier=io.github.edouard-claude.penelope\nAuthority=Penelope Dev\n\
                      Signed Time=17 sept. 2026\n";
        assert_eq!(
            parse_details(signed).describe(),
            "signé par « Penelope Dev » (io.github.edouard-claude.penelope)"
        );
        assert_eq!(
            parse_details("/x/penelope: code object is not signed at all"),
            Signature::Unsigned
        );
    }

    /// Le binaire de test lui-même : ad hoc (linker) ou signé, jamais inconnu sur macOS.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_running_binary_has_a_signature() {
        let exe = std::env::current_exe().unwrap();
        assert!(matches!(
            inspect(&exe),
            Signature::AdHoc { .. } | Signature::Identity { .. }
        ));
        assert!(designated_requirement(&exe).is_some());
    }
}

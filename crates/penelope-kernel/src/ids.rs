//! Identifiants stables : ULID (26 caractères, Crockford base32, triables dans le temps).
//!
//! Implémentation locale volontaire (pas de dépendance externe) : le format est figé
//! par le PRD (§6.4, `<!-- uid: 01J… -->`) et doit rester stable sur toute la durée de
//! vie des fichiers du vault.

use std::fmt;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Table inverse de Crockford base32 (accepte les minuscules, I/L vers 1, O vers 0).
fn decode_char(c: u8) -> Option<u8> {
    let c = c.to_ascii_uppercase();
    match c {
        b'0' | b'O' => Some(0),
        b'1' | b'I' | b'L' => Some(1),
        b'U' => None,
        _ => ALPHABET.iter().position(|&a| a == c).map(|p| p as u8),
    }
}

/// ULID : 48 bits d'horodatage (ms depuis l'epoch) + 80 bits aléatoires.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ulid(pub u128);

impl Ulid {
    /// Génère un ULID à partir de l'horloge système et d'aléa cryptographique.
    pub fn new() -> Self {
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        Self::from_parts(ms, random_80())
    }

    /// Construit un ULID déterministe (tests, rejeu).
    pub fn from_parts(ms: u64, randomness: u128) -> Self {
        let ts = (ms as u128 & ((1u128 << 48) - 1)) << 80;
        Ulid(ts | (randomness & ((1u128 << 80) - 1)))
    }

    /// Horodatage en millisecondes depuis l'epoch Unix.
    pub fn timestamp_ms(&self) -> u64 {
        (self.0 >> 80) as u64
    }

    pub fn to_string_upper(&self) -> String {
        let mut out = [0u8; 26];
        let mut v = self.0;
        for i in (0..26).rev() {
            out[i] = ALPHABET[(v & 0x1f) as usize];
            v >>= 5;
        }
        String::from_utf8(out.to_vec()).expect("alphabet ascii")
    }
}

impl Default for Ulid {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for Ulid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_string_upper())
    }
}

impl fmt::Debug for Ulid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Ulid({})", self.to_string_upper())
    }
}

impl FromStr for Ulid {
    type Err = UlidError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != 26 {
            return Err(UlidError::Length(s.len()));
        }
        let mut v: u128 = 0;
        for b in s.as_bytes() {
            let d = decode_char(*b).ok_or(UlidError::Char(*b as char))?;
            v = (v << 5) | d as u128;
        }
        Ok(Ulid(v))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UlidError {
    Length(usize),
    Char(char),
}

impl fmt::Display for UlidError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UlidError::Length(n) => write!(f, "ULID invalide : {n} caractères au lieu de 26"),
            UlidError::Char(c) => write!(f, "ULID invalide : caractère '{c}'"),
        }
    }
}

impl std::error::Error for UlidError {}

impl serde::Serialize for Ulid {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string_upper())
    }
}

impl<'de> serde::Deserialize<'de> for Ulid {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

fn random_80() -> u128 {
    let mut buf = [0u8; 16];
    // getrandom ne peut échouer que si l'OS n'a pas de source d'entropie : on dégrade
    // alors sur l'horloge nanoseconde plutôt que de paniquer.
    if getrandom::getrandom(&mut buf[6..]).is_err() {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        buf[12..16].copy_from_slice(&n.to_be_bytes());
    }
    u128::from_be_bytes(buf)
}

/// Jeton opaque court, base62, pour les `callback_data` Telegram (≤ 64 octets, §14.8).
pub fn short_token(len: usize) -> String {
    const B62: &[u8; 62] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut raw = vec![0u8; len];
    let _ = getrandom::getrandom(&mut raw);
    raw.iter()
        .map(|b| B62[(*b as usize) % 62] as char)
        .collect()
}

macro_rules! id_newtype {
    ($(#[$m:meta])* $name:ident, $prefix:literal) => {
        $(#[$m])*
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new() -> Self {
                $name(format!("{}{}", $prefix, Ulid::new()))
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl Default for $name {
            fn default() -> Self { Self::new() }
        }
        impl From<String> for $name {
            fn from(s: String) -> Self { $name(s) }
        }
        impl From<&str> for $name {
            fn from(s: &str) -> Self { $name(s.to_string()) }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }
    };
}

id_newtype!(/// Identifiant de session (`chat`, `workflow_run`, `sub_agent`, `scheduled`).
    SessionId, "s_");
id_newtype!(/// Identifiant d'exécution de workflow.
    RunId, "r_");
id_newtype!(/// Identifiant de tour dans la file durable.
    TurnId, "t_");
id_newtype!(/// Identifiant d'effet de bord dans le ledger.
    EffectId, "e_");
id_newtype!(/// Identifiant de demande d'approbation HITL.
    ApprovalId, "a_");
id_newtype!(/// Identifiant d'artefact externalisé.
    ArtifactId, "art_");
id_newtype!(/// Identifiant de nœud LCM.
    NodeId, "n_");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ulid_roundtrip() {
        let u = Ulid::from_parts(1_757_000_000_000, 0x0123_4567_89ab_cdef_0123);
        let s = u.to_string_upper();
        assert_eq!(s.len(), 26);
        assert_eq!(Ulid::from_str(&s).unwrap(), u);
    }

    #[test]
    fn ulid_is_monotonic_on_timestamp() {
        let a = Ulid::from_parts(1000, 0xffff_ffff_ffff_ffff_ffff);
        let b = Ulid::from_parts(1001, 0);
        assert!(a < b, "les ULID doivent trier par horodatage");
        assert!(
            a.to_string_upper() < b.to_string_upper(),
            "tri lexicographique = tri temporel"
        );
    }

    #[test]
    fn ulid_timestamp_extraction() {
        let u = Ulid::from_parts(1_757_000_000_000, 42);
        assert_eq!(u.timestamp_ms(), 1_757_000_000_000);
    }

    #[test]
    fn crockford_accepts_ambiguous_chars() {
        let u = Ulid::from_parts(5, 7);
        let s = u.to_string_upper().replace('0', "O");
        assert_eq!(Ulid::from_str(&s).unwrap(), u);
    }

    #[test]
    fn short_token_length() {
        assert_eq!(short_token(12).len(), 12);
    }

    /// Crockford base32 : minuscules acceptées, `O` lu 0, `I` et `L` lus 1, `U` refusé.
    #[test]
    fn crockford_decoding_is_forgiving_but_not_lax() {
        let u = Ulid::from_parts(1_757_000_000_000, 42);
        let s = u.to_string();
        assert_eq!(s.to_lowercase().parse::<Ulid>().unwrap(), u);
        assert_eq!(
            "O".repeat(26).parse::<Ulid>().unwrap(),
            "0".repeat(26).parse::<Ulid>().unwrap()
        );
        let ones = "1".repeat(26).parse::<Ulid>().unwrap();
        assert_eq!("I".repeat(26).parse::<Ulid>().unwrap(), ones);
        assert_eq!("l".repeat(26).parse::<Ulid>().unwrap(), ones);
        let e = "U".repeat(26).parse::<Ulid>().unwrap_err();
        assert_eq!(e, UlidError::Char('U'));
        assert_eq!(e.to_string(), "ULID invalide : caractère 'U'");
        let e = "01J".parse::<Ulid>().unwrap_err();
        assert_eq!(e.to_string(), "ULID invalide : 3 caractères au lieu de 26");
        assert_eq!(format!("{u:?}"), format!("Ulid({s})"));
    }

    /// Un ULID se sérialise en texte et refuse de se relire depuis un texte invalide.
    #[test]
    fn ulids_round_trip_through_json() {
        let u = Ulid::new();
        let j = serde_json::to_string(&u).unwrap();
        assert_eq!(j, format!("\"{u}\""));
        assert_eq!(serde_json::from_str::<Ulid>(&j).unwrap(), u);
        assert!(serde_json::from_str::<Ulid>("\"court\"").is_err());
        assert!(Ulid::default().timestamp_ms() > 1_700_000_000_000);
    }

    /// Les identifiants typés portent leur préfixe et se lisent comme du texte.
    #[test]
    fn typed_ids_carry_their_prefix() {
        assert!(RunId::default().as_str().starts_with("r_"));
        assert!(ArtifactId::new().as_str().starts_with("art_"));
        let t = TurnId::from("t_1");
        assert_eq!(t, TurnId::from("t_1".to_string()));
        assert_eq!(t.to_string(), "t_1");
        assert_eq!(format!("{t:?}"), "TurnId(t_1)");
        let tok = short_token(12);
        assert_eq!(tok.len(), 12);
        assert!(tok.chars().all(|c| c.is_ascii_alphanumeric()));
    }
}

//! Magasin de secrets (§2.4, §13.1).
//!
//! macOS : Trousseau via `/usr/bin/security` (exécutable, jamais un shell). Repli
//! headless commun à tous les OS : fichier chiffré XChaCha20-Poly1305, clé dérivée par
//! Argon2id d'une phrase de passe, ou clé brute fournie par `PENELOPE_MASTER_KEY_FILE`.
//!
//! La syntaxe de configuration est unique : `${SECRET:nom}`. `${KEYCHAIN:nom}` est
//! accepté comme alias historique.

use crate::{PlatformError, Result};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Découpage d'un secret trop long pour être écrit d'un bloc (issue #148).
///
/// `security -i`, sur macOS, n'accepte que 4 096 octets par ligne : au-delà, la fin de la
/// ligne est relue comme des commandes, et `security` en recopie des morceaux — donc du
/// secret — dans sa sortie d'erreur. Le `Grant` Codex (#142), premier secret de plusieurs
/// kilo-octets, a fait sortir cinq messages de jetons en clair sur Telegram.
///
/// La valeur est donc écrite en plusieurs items, et un item de tête dit combien. Un
/// secret court garde sa forme d'avant : la lecture reste compatible.
pub mod chunks {
    /// Longueur maximale d'une commande envoyée à `security -i` ; la limite réelle est de
    /// 4 096 octets, la marge couvre les variantes de version.
    pub const MAX_LINE_BYTES: usize = 4_000;
    /// Nombre de morceaux admis : 64 × ~1,9 Ko, bien au-delà du plus gros secret connu.
    pub const MAX_CHUNKS: usize = 64;
    /// Marque de l'item de tête d'un secret découpé.
    ///
    /// **Imprimable, et ce n'est pas un détail** (issue #157) : `security -w` ne rend un
    /// mot de passe en clair que s'il l'est. Dès qu'il porte un octet de contrôle, il
    /// l'imprime en hexadécimal — la tête revenait en 42 caractères que `get` prenait
    /// pour la valeur du secret. Une valeur qui commencerait par cette marque ne peut pas
    /// être confondue pour autant : `set` la force sur le chemin découpé.
    pub const MARK: &str = "penelope-chunks:v1:";

    /// La marque des versions 0.17.26 à 0.17.35, avec son caractère de contrôle en tête.
    /// Relue, jamais écrite : les secrets déjà posés dans le Trousseau restent lisibles.
    pub const LEGACY_MARK: &str = "\u{1}penelope-chunks:v1:";

    pub fn header(count: usize) -> String {
        format!("{MARK}{count}")
    }

    /// Nombre de morceaux annoncé par un item de tête, s'il en est un. Les deux marques
    /// sont acceptées ; l'ancienne est testée en second, la nouvelle étant son suffixe.
    pub fn count(head: &str) -> Option<usize> {
        head.strip_prefix(MARK)
            .or_else(|| head.strip_prefix(LEGACY_MARK))?
            .parse::<usize>()
            .ok()
            .filter(|n| *n > 0 && *n <= MAX_CHUNKS)
    }

    /// Décode une sortie de `security -w` qui est arrivée en hexadécimal (issue #157).
    ///
    /// N'accepte que ce qui redonne une tête de morceaux : une valeur entièrement
    /// hexadécimale est un secret parfaitement ordinaire (empreinte, clé brute), et la
    /// décoder au hasard rendrait faux ce qui était juste.
    pub fn decode_hex_head(raw: &str) -> Option<String> {
        if raw.len() < 2 || !raw.len().is_multiple_of(2) {
            return None;
        }
        if !raw.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let bytes: Vec<u8> = (0..raw.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&raw[i..i + 2], 16))
            .collect::<std::result::Result<_, _>>()
            .ok()?;
        let text = String::from_utf8(bytes).ok()?;
        count(&text).is_some().then_some(text)
    }

    /// Découpe une valeur en morceaux d'au plus `budget` octets, sans couper un caractère
    /// UTF-8 en deux.
    pub fn split(value: &str, budget: usize) -> Vec<String> {
        let budget = budget.max(1);
        let mut parts = Vec::new();
        let mut rest = value;
        while !rest.is_empty() {
            if rest.len() <= budget {
                parts.push(rest.to_string());
                break;
            }
            let mut cut = budget;
            while cut > 0 && !rest.is_char_boundary(cut) {
                cut -= 1;
            }
            let (head, tail) = rest.split_at(cut.max(1).min(rest.len()));
            parts.push(head.to_string());
            rest = tail;
        }
        parts
    }
}

pub trait SecretStore: Send + Sync {
    /// Nom du backend actif (`penelope secret backend`).
    fn backend(&self) -> String;
    fn get(&self, name: &str) -> Result<Option<String>>;
    fn set(&self, name: &str, value: &str) -> Result<()>;
    fn delete(&self, name: &str) -> Result<()>;
    fn list(&self) -> Result<Vec<String>>;

    /// Résout `${SECRET:nom}` et `${ENV:nom}` dans une valeur de configuration.
    ///
    /// Un secret introuvable est une **erreur** : mieux vaut un démarrage qui explique
    /// qu'un appel réseau avec un en-tête vide.
    fn expand(&self, raw: &str) -> Result<String> {
        let mut out = String::with_capacity(raw.len());
        let mut rest = raw;
        while let Some(start) = rest.find("${") {
            out.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            let Some(end) = after.find('}') else {
                out.push_str(&rest[start..]);
                return Ok(out);
            };
            let inner = &after[..end];
            let replacement = match inner.split_once(':') {
                Some(("SECRET", name)) | Some(("KEYCHAIN", name)) => {
                    self.get(name)?.ok_or_else(|| {
                        PlatformError::Secret(format!(
                            "secret `{name}` absent du magasin ({}). \
                             Le définir avec : penelope secret set {name}",
                            self.backend()
                        ))
                    })?
                }
                Some(("ENV", name)) => std::env::var(name).map_err(|_| {
                    PlatformError::Secret(format!("variable d'environnement `{name}` absente"))
                })?,
                _ => format!("${{{inner}}}"),
            };
            out.push_str(&replacement);
            rest = &after[end + 1..];
        }
        out.push_str(rest);
        Ok(out)
    }
}

/// Valide un nom de secret.
///
/// Plus large qu'un slug : les noms en usage portent des soulignés
/// (`openrouter_api_key`) et les fichiers `mcp.d` des majuscules (`FORGE_TOKEN`).
/// Rien d'autre n'est admis, pour que le nom reste sûr comme service du Trousseau et
/// comme clé du fichier chiffré.
pub fn validate_secret_name(name: &str) -> Result<()> {
    let ok_len = (1..=128).contains(&name.len());
    let ok_chars = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
    if !ok_len || !ok_chars || name.starts_with('.') {
        return Err(PlatformError::Secret(format!(
            "nom de secret invalide : `{name}` (attendu 1 à 128 caractères parmi \
             [A-Za-z0-9_.-], sans point initial)"
        )));
    }
    Ok(())
}

/// Repère les placeholders présents dans une valeur, sans les résoudre.
pub fn placeholders(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = raw;
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else { break };
        out.push(after[..end].to_string());
        rest = &after[end + 1..];
    }
    out
}

// ------------------------------------------------------------- fichier chiffré

const MAGIC: &[u8; 8] = b"PNLPSEC1";

/// Magasin de secrets en fichier chiffré : le repli headless.
pub struct EncryptedFileStore {
    path: PathBuf,
    key: [u8; 32],
}

impl EncryptedFileStore {
    /// Ouvre ou crée le magasin, clé dérivée d'une phrase de passe (Argon2id).
    pub fn with_passphrase(path: impl Into<PathBuf>, passphrase: &str) -> Result<Self> {
        let path = path.into();
        let salt = load_or_create_salt(&path)?;
        let key = derive_key(passphrase.as_bytes(), &salt)?;
        Ok(EncryptedFileStore { path, key })
    }

    /// Clé brute de 32 octets lue dans un fichier (`systemd-creds`, secret Docker).
    pub fn with_key_file(path: impl Into<PathBuf>, key_file: &Path) -> Result<Self> {
        let raw = std::fs::read(key_file)?;
        let key = if raw.len() == 32 {
            let mut k = [0u8; 32];
            k.copy_from_slice(&raw);
            k
        } else {
            // Une clé de longueur libre est condensée en 32 octets.
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(&raw);
            let d = h.finalize();
            let mut k = [0u8; 32];
            k.copy_from_slice(&d);
            k
        };
        Ok(EncryptedFileStore {
            path: path.into(),
            key,
        })
    }

    fn read_all(&self) -> Result<BTreeMap<String, String>> {
        if !self.path.exists() {
            return Ok(BTreeMap::new());
        }
        let raw = std::fs::read(&self.path)?;
        if raw.len() < MAGIC.len() + 24 {
            return Err(PlatformError::Secret(
                "fichier de secrets tronqué ou corrompu".into(),
            ));
        }
        if &raw[..8] != MAGIC {
            return Err(PlatformError::Secret(
                "fichier de secrets : en-tête inattendu".into(),
            ));
        }
        let nonce = XNonce::from_slice(&raw[8..32]);
        let cipher = XChaCha20Poly1305::new_from_slice(&self.key)
            .map_err(|e| PlatformError::Secret(e.to_string()))?;
        let plain = cipher.decrypt(nonce, &raw[32..]).map_err(|_| {
            PlatformError::Secret(
                "déchiffrement impossible : phrase de passe ou clé incorrecte".into(),
            )
        })?;
        Ok(serde_json::from_slice(&plain)?)
    }

    fn write_all(&self, map: &BTreeMap<String, String>) -> Result<()> {
        let plain = serde_json::to_vec(map)?;
        let mut nonce_bytes = [0u8; 24];
        getrandom::getrandom(&mut nonce_bytes)
            .map_err(|e| PlatformError::Secret(format!("entropie indisponible : {e}")))?;
        let nonce = XNonce::from_slice(&nonce_bytes);
        let cipher = XChaCha20Poly1305::new_from_slice(&self.key)
            .map_err(|e| PlatformError::Secret(e.to_string()))?;
        let ct = cipher
            .encrypt(nonce, plain.as_ref())
            .map_err(|e| PlatformError::Secret(e.to_string()))?;

        let mut out = Vec::with_capacity(32 + ct.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ct);

        if let Some(p) = self.path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, &out)?;
        restrict_permissions(&tmp)?;
        std::fs::rename(&tmp, &self.path)?;
        restrict_permissions(&self.path)?;
        Ok(())
    }
}

impl SecretStore for EncryptedFileStore {
    fn backend(&self) -> String {
        format!("fichier chiffré ({})", self.path.display())
    }

    fn get(&self, name: &str) -> Result<Option<String>> {
        Ok(self.read_all()?.get(name).cloned())
    }

    fn set(&self, name: &str, value: &str) -> Result<()> {
        let mut m = self.read_all()?;
        m.insert(name.to_string(), value.to_string());
        self.write_all(&m)
    }

    fn delete(&self, name: &str) -> Result<()> {
        let mut m = self.read_all()?;
        m.remove(name);
        self.write_all(&m)
    }

    fn list(&self) -> Result<Vec<String>> {
        Ok(self.read_all()?.keys().cloned().collect())
    }
}

fn salt_path(secrets: &Path) -> PathBuf {
    secrets.with_extension("salt")
}

fn load_or_create_salt(secrets: &Path) -> Result<[u8; 16]> {
    let p = salt_path(secrets);
    if let Ok(raw) = std::fs::read(&p)
        && raw.len() == 16
    {
        let mut s = [0u8; 16];
        s.copy_from_slice(&raw);
        return Ok(s);
    }
    let mut s = [0u8; 16];
    getrandom::getrandom(&mut s)
        .map_err(|e| PlatformError::Secret(format!("entropie indisponible : {e}")))?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&p, s)?;
    restrict_permissions(&p)?;
    Ok(s)
}

fn derive_key(passphrase: &[u8], salt: &[u8; 16]) -> Result<[u8; 32]> {
    use argon2::{Algorithm, Argon2, Params, Version};
    let params = Params::new(19 * 1024, 2, 1, Some(32))
        .map_err(|e| PlatformError::Secret(format!("paramètres Argon2 invalides : {e}")))?;
    let a2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = [0u8; 32];
    a2.hash_password_into(passphrase, salt, &mut key)
        .map_err(|e| PlatformError::Secret(format!("dérivation de clé : {e}")))?;
    Ok(key)
}

/// Permissions `0600` sur Unix ; sous Windows, l'ACL restreinte est posée par le backend.
pub fn restrict_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = std::fs::metadata(path)?.permissions();
        perm.set_mode(0o600);
        std::fs::set_permissions(path, perm)?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// Magasin en mémoire, pour les suites déterministes.
#[derive(Default)]
pub struct MemorySecretStore {
    map: std::sync::Mutex<BTreeMap<String, String>>,
}

impl MemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with(entries: &[(&str, &str)]) -> Self {
        let s = Self::new();
        for (k, v) in entries {
            let _ = s.set(k, v);
        }
        s
    }
}

impl SecretStore for MemorySecretStore {
    fn backend(&self) -> String {
        "mémoire (tests)".into()
    }
    fn get(&self, name: &str) -> Result<Option<String>> {
        Ok(self
            .map
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(name)
            .cloned())
    }
    fn set(&self, name: &str, value: &str) -> Result<()> {
        self.map
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(name.into(), value.into());
        Ok(())
    }
    fn delete(&self, name: &str) -> Result<()> {
        self.map
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(name);
        Ok(())
    }
    fn list(&self) -> Result<Vec<String>> {
        Ok(self
            .map
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .keys()
            .cloned()
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_names_accept_what_the_project_actually_uses() {
        for ok in [
            "openrouter_api_key",
            "telegram_bot_token",
            "FORGE_TOKEN",
            "redmine.api-key",
        ] {
            validate_secret_name(ok).unwrap_or_else(|e| panic!("refusé à tort : {ok} → {e}"));
        }
        for bad in [
            "",
            ".cache",
            "pas un nom",
            "clé",
            "a/b",
            "$(whoami)",
            &"x".repeat(129),
        ] {
            assert!(
                validate_secret_name(bad).is_err(),
                "accepté à tort : {bad:?}"
            );
        }
    }

    /// #148 : un secret de plusieurs kilo-octets passe la borne d'une ligne `security`
    /// en morceaux, et une valeur courte garde la forme d'avant (lecture compatible).
    #[test]
    fn a_long_value_is_cut_into_readable_chunks() {
        use chunks::{MAX_CHUNKS, count, header, split};

        // L'en-tête se relit, et rien d'autre ne passe pour un en-tête.
        assert_eq!(count(&header(3)), Some(3));
        for bad in [
            "",
            "sk-or-v1-abcdef",
            "{\"access_token\":\"…\"}",
            &header(0),
        ] {
            assert_eq!(count(bad), None, "`{bad}` n'est pas un en-tête");
        }
        assert_eq!(count(&header(MAX_CHUNKS + 1)), None, "borne du nombre");

        // Découpe : morceaux bornés, concaténation fidèle.
        let value = "a".repeat(16 * 1024);
        let parts = split(&value, 1_800);
        assert_eq!(parts.len(), 10);
        assert!(parts.iter().all(|p| p.len() <= 1_800));
        assert_eq!(parts.concat(), value);

        // Jamais un caractère UTF-8 coupé en deux : « é » fait deux octets.
        let accents = "é".repeat(100);
        let parts = split(&accents, 7);
        assert!(parts.iter().all(|p| p.len() <= 7));
        assert_eq!(parts.concat(), accents, "reconstruction exacte");
    }

    /// #157 : la marque d'en-tête doit être **imprimable**. `security -w` rend en
    /// hexadécimal tout mot de passe qui ne l'est pas : la tête revenait en 42 caractères
    /// que `get` prenait pour la valeur du secret.
    #[test]
    fn the_chunk_mark_is_printable_and_the_old_one_is_still_read() {
        use chunks::{LEGACY_MARK, MARK, count, decode_hex_head, header};

        assert!(
            MARK.chars().all(|c| !c.is_control()),
            "aucun caractère de contrôle : {MARK:?}"
        );
        assert_eq!(header(3), "penelope-chunks:v1:3");

        // L'ancienne marque reste lue : les secrets déjà posés n'ont pas à migrer.
        assert_eq!(count(&format!("{LEGACY_MARK}3")), Some(3));
        assert_eq!(count(&header(3)), Some(3));

        // L'hexadécimal que rendait le vrai `security` pour l'ancienne tête.
        let hexa: String = format!("{LEGACY_MARK}3")
            .bytes()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(hexa, "0170656e656c6f70652d6368756e6b733a76313a33");
        assert_eq!(hexa.len(), 42, "les 42 caractères du rapport de doctor");
        assert_eq!(count(&hexa), None, "ce n'est pas un en-tête tel quel");
        assert_eq!(
            decode_hex_head(&hexa).as_deref(),
            Some(format!("{LEGACY_MARK}3").as_str()),
            "mais il se décode en un en-tête"
        );

        // Ce qui ne redonne pas un en-tête n'est jamais décodé : une empreinte, une clé
        // brute, un jeton hexadécimal sont des secrets parfaitement ordinaires.
        for ordinary in [
            "deadbeef",
            "0123456789abcdef0123456789abcdef",
            &"a".repeat(64),
        ] {
            assert_eq!(
                decode_hex_head(ordinary),
                None,
                "`{ordinary}` reste tel quel"
            );
        }
        // Ni ce qui n'est pas de l'hexadécimal, ou de longueur impaire.
        assert_eq!(decode_hex_head("sk-or-v1-abcdef"), None);
        assert_eq!(decode_hex_head("0170656"), None);
        assert_eq!(decode_hex_head(""), None);
    }

    /// #148 : le contrat d'un magasin de secrets ne dépend pas de la taille de la valeur.
    /// Le `Grant` Codex fait 4 Ko ; ce test en prend 16.
    #[test]
    fn a_store_holds_a_sixteen_kilobyte_secret() {
        let s = MemorySecretStore::new();
        let big = format!(
            "{{\"access_token\":\"{}\",\"refresh_token\":\"{}\"}}",
            "e".repeat(8 * 1024),
            "r".repeat(8 * 1024)
        );
        s.set("codex.oauth", &big).unwrap();
        assert_eq!(s.get("codex.oauth").unwrap().as_deref(), Some(big.as_str()));
        assert_eq!(s.list().unwrap(), ["codex.oauth"], "un seul nom logique");
        s.delete("codex.oauth").unwrap();
        assert_eq!(s.get("codex.oauth").unwrap(), None);
    }

    #[test]
    fn encrypted_store_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("secrets.enc");
        let s = EncryptedFileStore::with_passphrase(&p, "phrase de passe correcte").unwrap();
        s.set("openrouter_api_key", "sk-or-v1-secret").unwrap();
        s.set("telegram_bot_token", "123:abc").unwrap();

        assert_eq!(
            s.get("openrouter_api_key").unwrap().as_deref(),
            Some("sk-or-v1-secret")
        );
        let mut names = s.list().unwrap();
        names.sort();
        assert_eq!(names, vec!["openrouter_api_key", "telegram_bot_token"]);

        s.delete("telegram_bot_token").unwrap();
        assert!(s.get("telegram_bot_token").unwrap().is_none());
    }

    #[test]
    fn file_is_not_readable_as_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("secrets.enc");
        let s = EncryptedFileStore::with_passphrase(&p, "pp").unwrap();
        s.set("k", "valeur-ultra-secrete").unwrap();
        let raw = std::fs::read(&p).unwrap();
        assert!(
            !String::from_utf8_lossy(&raw).contains("valeur-ultra-secrete"),
            "le fichier ne doit contenir aucun clair"
        );
    }

    #[test]
    fn wrong_passphrase_fails_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("secrets.enc");
        EncryptedFileStore::with_passphrase(&p, "bonne")
            .unwrap()
            .set("k", "v")
            .unwrap();
        let bad = EncryptedFileStore::with_passphrase(&p, "mauvaise").unwrap();
        let e = bad.get("k").unwrap_err().to_string();
        assert!(e.contains("phrase de passe"), "{e}");
    }

    #[cfg(unix)]
    #[test]
    fn permissions_are_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("secrets.enc");
        let s = EncryptedFileStore::with_passphrase(&p, "pp").unwrap();
        s.set("k", "v").unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let smode = std::fs::metadata(salt_path(&p))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(smode & 0o777, 0o600);
    }

    #[test]
    fn key_file_backend() {
        let dir = tempfile::tempdir().unwrap();
        let kf = dir.path().join("master.key");
        std::fs::write(&kf, [7u8; 32]).unwrap();
        let p = dir.path().join("secrets.enc");
        let s = EncryptedFileStore::with_key_file(&p, &kf).unwrap();
        s.set("a", "b").unwrap();
        let s2 = EncryptedFileStore::with_key_file(&p, &kf).unwrap();
        assert_eq!(s2.get("a").unwrap().as_deref(), Some("b"));
    }

    #[test]
    fn expand_secret_and_env_placeholders() {
        let s = MemorySecretStore::with(&[("telegram_bot_token", "123:abc")]);
        assert_eq!(
            s.expand("Bearer ${SECRET:telegram_bot_token}").unwrap(),
            "Bearer 123:abc"
        );
        // Alias historique.
        assert_eq!(
            s.expand("${KEYCHAIN:telegram_bot_token}").unwrap(),
            "123:abc"
        );
        // Placeholder inconnu : laissé tel quel.
        assert_eq!(s.expand("${AUTRE:x}").unwrap(), "${AUTRE:x}");
    }

    #[test]
    fn missing_secret_is_an_explicit_error() {
        let s = MemorySecretStore::new();
        let e = s.expand("${SECRET:absent}").unwrap_err().to_string();
        assert!(e.contains("penelope secret set absent"), "{e}");
    }

    #[test]
    fn placeholders_are_listed_without_resolution() {
        assert_eq!(
            placeholders("a ${SECRET:x} b ${ENV:Y} c"),
            vec!["SECRET:x", "ENV:Y"]
        );
    }

    #[test]
    fn expand_handles_unterminated_placeholder() {
        let s = MemorySecretStore::new();
        assert_eq!(s.expand("valeur ${SECRET:x").unwrap(), "valeur ${SECRET:x");
    }
}

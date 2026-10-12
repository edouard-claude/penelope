//! Archive chiffrée par phrase de passe (issues #42, #341).
//!
//! Même construction que le magasin de secrets : clé dérivée par Argon2id, chiffrement
//! XChaCha20-Poly1305. Le sel et le préfixe de nonce voyagent en tête du fichier, si bien
//! qu'une sauvegarde se restaure sur une machine neuve avec la seule phrase de passe.
//!
//! ```text
//!  PNLPBK02 │ sel (16) │ préfixe de nonce (19) │ bloc 0 │ bloc 1 │ … │ dernier bloc
//! ```
//!
//! Chiffrement en flux (#341), construction STREAM : le clair est découpé en blocs de
//! [`CHUNK`] octets, chacun scellé avec sa marque d'authenticité sous le nonce
//! `préfixe ‖ compteur (32 bits) ‖ dernier (1 octet)`, l'en-tête en données associées.
//! Le dernier bloc est le seul plus court que [`CHUNK`] (vide si le clair tombe juste) :
//! un bloc retiré, permuté ou une archive coupée font échouer le déchiffrement. La
//! mémoire reste bornée à deux blocs, quelle que soit la taille de la sauvegarde ; il n'y
//! a donc plus de plafond de taille.
//!
//! Aucune archive en clair n'est écrite à côté : le chiffrement lit le `tar.gz` de travail
//! et le déchiffrement se lit comme un flux ([`open_reader`]), que `tar` consomme.
//!
//! Le format d'avant (`PNLPBK01`, un seul bloc chiffré en mémoire, au plus 512 Mo) se lit
//! toujours : les archives déjà envoyées se restaurent.

use crate::{PlatformError, Result};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use std::io::{Read, Write};
use std::path::Path;

/// En-tête du format en flux (#341).
const MAGIC: &[u8; 8] = b"PNLPBK02";
/// En-tête du format d'un bloc (#42), lu à la restauration seulement.
const MAGIC_V1: &[u8; 8] = b"PNLPBK01";
/// Taille d'un bloc de clair.
pub const CHUNK: usize = 1024 * 1024;
const TAG: usize = 16;
const PREFIX: usize = 19;
const HEADER: usize = 8 + 16 + PREFIX;

fn derive(passphrase: &str, salt: &[u8; 16]) -> Result<[u8; 32]> {
    use argon2::{Algorithm, Argon2, Params, Version};
    let params = Params::new(19 * 1024, 2, 1, Some(32))
        .map_err(|e| PlatformError::Secret(format!("paramètres Argon2 invalides : {e}")))?;
    let a2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = [0u8; 32];
    a2.hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|e| PlatformError::Secret(format!("dérivation de clé : {e}")))?;
    Ok(key)
}

fn refuse_empty(passphrase: &str) -> Result<()> {
    if passphrase.trim().is_empty() {
        return Err(PlatformError::Secret(
            "phrase de passe vide : l'archive ne serait pas protégée".into(),
        ));
    }
    Ok(())
}

fn random<const N: usize>() -> Result<[u8; N]> {
    let mut b = [0u8; N];
    getrandom::getrandom(&mut b)
        .map_err(|e| PlatformError::Secret(format!("entropie indisponible : {e}")))?;
    Ok(b)
}

fn cipher_for(passphrase: &str, salt: &[u8; 16]) -> Result<XChaCha20Poly1305> {
    let key = derive(passphrase, salt)?;
    XChaCha20Poly1305::new_from_slice(&key).map_err(|e| PlatformError::Secret(e.to_string()))
}

/// En-tête des secrets rangés dans l'archive (#327) : une seconde couche, son propre sel.
const SECRETS_MAGIC: &[u8; 8] = b"PNLPSK01";

/// Chiffre `plain` derrière l'en-tête `magic` : sel et nonce tirés au sort, clé dérivée de
/// la phrase de passe. La phrase de passe vide est refusée.
fn seal_with(magic: &[u8; 8], plain: &[u8], passphrase: &str) -> Result<Vec<u8>> {
    refuse_empty(passphrase)?;
    let salt = random::<16>()?;
    let nonce_bytes = random::<24>()?;
    let ct = cipher_for(passphrase, &salt)?
        .encrypt(XNonce::from_slice(&nonce_bytes), plain)
        .map_err(|e| PlatformError::Secret(e.to_string()))?;
    let mut out = Vec::with_capacity(48 + ct.len());
    out.extend_from_slice(magic);
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Déchiffre ce que [`seal_with`] a produit sous le même en-tête. `what` nomme la source
/// dans l'erreur.
fn open_with(magic: &[u8; 8], raw: &[u8], passphrase: &str, what: &str) -> Result<Vec<u8>> {
    if raw.len() < 48 || &raw[..8] != magic {
        return Err(not_a_backup(what));
    }
    let mut salt = [0u8; 16];
    salt.copy_from_slice(&raw[8..24]);
    cipher_for(passphrase, &salt)?
        .decrypt(XNonce::from_slice(&raw[24..48]), &raw[48..])
        .map_err(|_| wrong_passphrase())
}

fn not_a_backup(what: &str) -> PlatformError {
    PlatformError::Secret(format!("{what} n'est pas une sauvegarde Pénélope"))
}

fn wrong_passphrase() -> PlatformError {
    PlatformError::Secret("déchiffrement impossible : phrase de passe incorrecte".into())
}

/// Nonce du bloc `counter` : préfixe tiré au sort, compteur, marque du dernier bloc.
fn chunk_nonce(prefix: &[u8; PREFIX], counter: u32, last: bool) -> XNonce {
    let mut n = [0u8; 24];
    n[..PREFIX].copy_from_slice(prefix);
    n[PREFIX..PREFIX + 4].copy_from_slice(&counter.to_be_bytes());
    n[23] = u8::from(last);
    *XNonce::from_slice(&n)
}

/// Lit jusqu'à remplir `buf` ou atteindre la fin : rend le nombre d'octets lus.
fn read_full(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// Chiffre `src` vers `dst`, bloc par bloc : la taille n'est pas bornée, la mémoire si.
/// La phrase de passe vide est refusée : une archive lisible par tous n'est pas une
/// sauvegarde. Un échec en route retire `dst` : rien d'incomplet ne reste.
pub fn seal(src: &Path, dst: &Path, passphrase: &str) -> Result<u64> {
    refuse_empty(passphrase)?;
    let mut input = std::fs::File::open(src)?;
    if let Some(p) = dst.parent() {
        std::fs::create_dir_all(p)?;
    }
    let file = std::fs::File::create(dst)?;
    // Restreint avant d'écrire : l'archive n'est jamais lisible par d'autres, même
    // partielle.
    crate::secrets::restrict_permissions(dst)?;
    let result = seal_stream(&mut input, std::io::BufWriter::new(file), passphrase);
    if result.is_err() {
        let _ = std::fs::remove_file(dst);
    }
    result
}

fn seal_stream(input: &mut impl Read, mut out: impl Write, passphrase: &str) -> Result<u64> {
    let salt = random::<16>()?;
    let prefix = random::<PREFIX>()?;
    let cipher = cipher_for(passphrase, &salt)?;
    let mut header = [0u8; HEADER];
    header[..8].copy_from_slice(MAGIC);
    header[8..24].copy_from_slice(&salt);
    header[24..].copy_from_slice(&prefix);
    out.write_all(&header)?;
    let mut written = HEADER as u64;
    let mut buf = vec![0u8; CHUNK];
    let mut counter: u32 = 0;
    loop {
        let n = read_full(input, &mut buf)?;
        let last = n < CHUNK;
        let payload = Payload {
            msg: &buf[..n],
            aad: &header,
        };
        let ct = cipher
            .encrypt(&chunk_nonce(&prefix, counter, last), payload)
            .map_err(|e| PlatformError::Secret(e.to_string()))?;
        out.write_all(&ct)?;
        written += ct.len() as u64;
        if last {
            break;
        }
        counter = counter.checked_add(1).ok_or_else(|| {
            PlatformError::Secret("archive trop grande pour le compteur de blocs".into())
        })?;
    }
    out.flush()?;
    Ok(written)
}

/// Le clair d'une archive, lu bloc par bloc : chaque bloc est authentifié avant d'être
/// rendu, et une archive coupée ou retouchée rend une erreur de lecture (`InvalidData`)
/// au lieu d'une fin. Le lecteur (`tar`) doit donc aller jusqu'au bout et faire remonter
/// l'erreur.
struct StreamReader<R> {
    input: R,
    cipher: XChaCha20Poly1305,
    header: [u8; HEADER],
    counter: u32,
    sealed: Vec<u8>,
    plain: Vec<u8>,
    pos: usize,
    done: bool,
}

impl<R: Read> StreamReader<R> {
    /// Ouvre le bloc suivant. Le premier qui échoue dit la phrase de passe incorrecte ;
    /// un bloc suivant, une archive altérée.
    fn next_chunk(&mut self) -> Result<()> {
        let n = read_full(&mut self.input, &mut self.sealed)?;
        if n < TAG {
            return Err(PlatformError::Secret(
                "archive tronquée : le dernier bloc manque".into(),
            ));
        }
        let last = n < self.sealed.len();
        let mut prefix = [0u8; PREFIX];
        prefix.copy_from_slice(&self.header[24..]);
        let payload = Payload {
            msg: &self.sealed[..n],
            aad: &self.header,
        };
        self.plain = self
            .cipher
            .decrypt(&chunk_nonce(&prefix, self.counter, last), payload)
            .map_err(|_| {
                if self.counter == 0 {
                    wrong_passphrase()
                } else {
                    PlatformError::Secret(format!(
                        "archive altérée ou tronquée (bloc {})",
                        self.counter
                    ))
                }
            })?;
        self.pos = 0;
        self.done = last;
        if !last {
            self.counter = self.counter.checked_add(1).ok_or_else(|| {
                PlatformError::Secret("archive trop grande pour le compteur de blocs".into())
            })?;
        }
        Ok(())
    }
}

impl<R: Read> Read for StreamReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while self.pos == self.plain.len() {
            if self.done {
                return Ok(0);
            }
            self.next_chunk().map_err(|e| match e {
                PlatformError::Io(io) => io,
                other => std::io::Error::new(std::io::ErrorKind::InvalidData, other.to_string()),
            })?;
        }
        let n = buf.len().min(self.plain.len() - self.pos);
        buf[..n].copy_from_slice(&self.plain[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// Ouvre `src` en lecture du clair, sans rien écrire. Le premier bloc est déchiffré tout
/// de suite : une phrase de passe incorrecte ou un fichier étranger sont dits ici, pas au
/// milieu de la lecture. Le contrôle de chaque sauvegarde (#328) et la restauration
/// (#329) passent par là.
pub fn open_reader(src: &Path, passphrase: &str) -> Result<Box<dyn Read + Send>> {
    let what = src.display().to_string();
    let mut input = std::io::BufReader::new(std::fs::File::open(src)?);
    let mut magic = [0u8; 8];
    if read_full(&mut input, &mut magic)? < 8 {
        return Err(not_a_backup(&what));
    }
    if &magic == MAGIC_V1 {
        // Format d'un bloc : au plus 512 Mo par construction, lu d'un coup.
        let mut raw = magic.to_vec();
        input.read_to_end(&mut raw)?;
        let plain = open_with(MAGIC_V1, &raw, passphrase, &what)?;
        return Ok(Box::new(std::io::Cursor::new(plain)));
    }
    if &magic != MAGIC {
        return Err(not_a_backup(&what));
    }
    let mut header = [0u8; HEADER];
    header[..8].copy_from_slice(MAGIC);
    if read_full(&mut input, &mut header[8..])? < HEADER - 8 {
        return Err(not_a_backup(&what));
    }
    let mut salt = [0u8; 16];
    salt.copy_from_slice(&header[8..24]);
    let mut reader = StreamReader {
        input,
        cipher: cipher_for(passphrase, &salt)?,
        header,
        counter: 0,
        sealed: vec![0u8; CHUNK + TAG],
        plain: Vec::new(),
        pos: 0,
        done: false,
    };
    reader.next_chunk()?;
    Ok(Box::new(reader))
}

/// Déchiffre `src` en mémoire : pour les petites archives et les tests ; une sauvegarde
/// se lit par [`open_reader`].
pub fn open_to_vec(src: &Path, passphrase: &str) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    open_reader(src, passphrase)?.read_to_end(&mut out)?;
    Ok(out)
}

/// Déchiffre `src` vers `dst`, en flux. Une phrase de passe incorrecte est dite comme
/// telle ; une archive altérée ne laisse pas de `dst` partiel.
pub fn open(src: &Path, dst: &Path, passphrase: &str) -> Result<u64> {
    let mut reader = open_reader(src, passphrase)?;
    if let Some(p) = dst.parent() {
        std::fs::create_dir_all(p)?;
    }
    let mut out = std::fs::File::create(dst)?;
    let copied = std::io::copy(&mut reader, &mut out);
    match copied {
        Ok(n) => Ok(n),
        Err(e) => {
            drop(out);
            let _ = std::fs::remove_file(dst);
            Err(e.into())
        }
    }
}

/// Chiffre les secrets rangés dans l'archive (#327) : une couche de plus que l'archive,
/// dérivée de la même phrase de passe avec son propre sel. Un tar extrait par erreur ne
/// livre ainsi aucune valeur.
pub fn seal_secrets(plain: &[u8], passphrase: &str) -> Result<Vec<u8>> {
    seal_with(SECRETS_MAGIC, plain, passphrase)
}

/// Déchiffre les secrets d'une archive.
pub fn open_secrets(raw: &[u8], passphrase: &str) -> Result<Vec<u8>> {
    open_with(SECRETS_MAGIC, raw, passphrase, "le fichier des secrets")
}

#[cfg(test)]
mod tests;

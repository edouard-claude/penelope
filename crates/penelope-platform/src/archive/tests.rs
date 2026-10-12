use super::*;

#[test]
fn sealed_archive_reopens_with_its_passphrase_only() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("sauvegarde.tar.gz");
    std::fs::write(&src, b"contenu de la sauvegarde, pas du bruit").unwrap();
    let sealed = dir.path().join("sauvegarde.tar.gz.enc");
    seal(&src, &sealed, "phrase de passe correcte").unwrap();

    // Rien du contenu ne se lit dans le fichier chiffré.
    let raw = std::fs::read(&sealed).unwrap();
    assert!(raw.starts_with(MAGIC));
    assert!(
        !String::from_utf8_lossy(&raw).contains("contenu de la sauvegarde"),
        "l'archive ne doit pas être lisible"
    );

    let back = dir.path().join("rendu.tar.gz");
    open(&sealed, &back, "phrase de passe correcte").unwrap();
    assert_eq!(
        std::fs::read(&back).unwrap(),
        b"contenu de la sauvegarde, pas du bruit"
    );

    let e = open(&sealed, &back, "mauvaise phrase").unwrap_err();
    assert!(e.to_string().contains("phrase de passe incorrecte"), "{e}");
}

#[test]
fn an_empty_passphrase_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("a.tar.gz");
    std::fs::write(&src, b"x").unwrap();
    let e = seal(&src, &dir.path().join("a.enc"), "   ").unwrap_err();
    assert!(e.to_string().contains("phrase de passe vide"), "{e}");
}

#[test]
fn a_foreign_file_is_not_mistaken_for_a_backup() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("autre.bin");
    std::fs::write(&src, b"ceci n'est pas une sauvegarde de Penelope du tout").unwrap();
    let e = open(&src, &dir.path().join("out"), "x").unwrap_err();
    assert!(e.to_string().contains("n'est pas une sauvegarde"), "{e}");
}

/// #327 : les secrets ont leur propre couche ; l'en-tête de l'archive n'ouvre pas
/// celle des secrets, et la mauvaise phrase est dite.
#[test]
fn secrets_have_their_own_layer() {
    let sealed = seal_secrets(br#"{"cle":"valeur-secrete"}"#, "phrase").unwrap();
    assert!(!String::from_utf8_lossy(&sealed).contains("valeur-secrete"));
    assert_eq!(
        open_secrets(&sealed, "phrase").unwrap(),
        br#"{"cle":"valeur-secrete"}"#
    );
    let e = open_secrets(&sealed, "autre").unwrap_err();
    assert!(e.to_string().contains("phrase de passe incorrecte"), "{e}");
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("s.enc");
    std::fs::write(&f, &sealed).unwrap();
    let e = open_to_vec(&f, "phrase").unwrap_err();
    assert!(e.to_string().contains("n'est pas une sauvegarde"), "{e}");
}

/// Un clair pseudo-aléatoire et reproductible de `len` octets.
fn noise(len: usize) -> Vec<u8> {
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

fn sealed_of(dir: &Path, plain: &[u8]) -> std::path::PathBuf {
    let src = dir.join("clair");
    std::fs::write(&src, plain).unwrap();
    let sealed = dir.join("chiffre.enc");
    seal(&src, &sealed, "phrase").unwrap();
    sealed
}

/// #341 : vide, plus court qu'un bloc, pile un bloc, plusieurs blocs et un reste : chaque
/// taille revient à l'identique, et l'archive pèse le clair plus une marque par bloc.
#[test]
fn every_size_round_trips_block_by_block() {
    for len in [0, 1, CHUNK - 1, CHUNK, CHUNK + 1, 3 * CHUNK + 12_345] {
        let dir = tempfile::tempdir().unwrap();
        let plain = noise(len);
        let sealed = sealed_of(dir.path(), &plain);
        let blocks = len / CHUNK + 1;
        assert_eq!(
            std::fs::metadata(&sealed).unwrap().len() as usize,
            HEADER + len + blocks * TAG,
            "taille chiffrée pour {len}"
        );
        assert_eq!(open_to_vec(&sealed, "phrase").unwrap(), plain, "{len}");
    }
}

/// #341 : la lecture ne tient jamais plus d'un bloc de clair et d'un bloc chiffré, quelle
/// que soit la taille de l'archive.
#[test]
fn reading_holds_one_block_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let sealed = sealed_of(dir.path(), &noise(5 * CHUNK + 7));
    let input = std::io::BufReader::new(std::fs::File::open(&sealed).unwrap());
    let mut header = [0u8; HEADER];
    header.copy_from_slice(&std::fs::read(&sealed).unwrap()[..HEADER]);
    let mut salt = [0u8; 16];
    salt.copy_from_slice(&header[8..24]);
    let mut input = input;
    let mut skip = [0u8; HEADER];
    input.read_exact(&mut skip).unwrap();
    let mut r = StreamReader {
        input,
        cipher: cipher_for("phrase", &salt).unwrap(),
        header,
        counter: 0,
        sealed: vec![0u8; CHUNK + TAG],
        plain: Vec::new(),
        pos: 0,
        done: false,
    };
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0;
    loop {
        let n = r.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        total += n;
        assert!(r.plain.capacity() <= CHUNK + TAG, "{}", r.plain.capacity());
        assert_eq!(r.sealed.capacity(), CHUNK + TAG);
    }
    assert_eq!(total, 5 * CHUNK + 7);
}

fn read_all(sealed: &Path) -> std::result::Result<Vec<u8>, String> {
    let mut out = Vec::new();
    open_reader(sealed, "phrase")
        .map_err(|e| e.to_string())?
        .read_to_end(&mut out)
        .map_err(|e| e.to_string())?;
    Ok(out)
}

/// #341 : une archive coupée, au milieu d'un bloc comme pile entre deux, n'est jamais lue
/// comme une archive plus courte.
#[test]
fn a_truncated_archive_fails() {
    let dir = tempfile::tempdir().unwrap();
    let sealed = sealed_of(dir.path(), &noise(3 * CHUNK + 100));
    let raw = std::fs::read(&sealed).unwrap();
    let full = CHUNK + TAG;
    for cut in [
        HEADER + full + 10,
        HEADER + 2 * full,
        HEADER + 3 * full,
        raw.len() - 1,
    ] {
        std::fs::write(&sealed, &raw[..cut]).unwrap();
        let e = read_all(&sealed).unwrap_err();
        assert!(
            e.contains("tronquée") || e.contains("altérée"),
            "coupe à {cut} : {e}"
        );
    }
}

/// #341 : un bloc retiré, deux blocs permutés ou un octet ajouté en fin font échouer la
/// lecture ; un en-tête retouché aussi.
#[test]
fn removed_swapped_or_appended_blocks_fail() {
    let dir = tempfile::tempdir().unwrap();
    let sealed = sealed_of(dir.path(), &noise(3 * CHUNK + 100));
    let raw = std::fs::read(&sealed).unwrap();
    let full = CHUNK + TAG;
    let block = |i: usize| &raw[HEADER + i * full..HEADER + (i + 1) * full];
    let tail = &raw[HEADER + 3 * full..];
    let head = &raw[..HEADER];

    let removed = [head, block(0), block(2), tail].concat();
    let swapped = [head, block(0), block(2), block(1), tail].concat();
    let appended = [raw.as_slice(), b"x"].concat();
    let mut retouched = raw.clone();
    retouched[30] ^= 1;
    for (what, bytes) in [
        ("retiré", removed),
        ("permuté", swapped),
        ("ajouté", appended),
        ("en-tête", retouched),
    ] {
        std::fs::write(&sealed, &bytes).unwrap();
        assert!(read_all(&sealed).is_err(), "bloc {what} accepté");
    }
}

/// #341 : une archive `PNLPBK01` (un bloc, avant le flux) se lit toujours ; les
/// archives déjà envoyées se restaurent.
#[test]
fn a_v1_archive_still_opens() {
    let dir = tempfile::tempdir().unwrap();
    let plain = noise(10_000);
    let v1 = dir.path().join("ancienne.tar.gz.enc");
    std::fs::write(&v1, seal_with(MAGIC_V1, &plain, "phrase").unwrap()).unwrap();
    assert_eq!(open_to_vec(&v1, "phrase").unwrap(), plain);
    let e = open_to_vec(&v1, "autre").unwrap_err();
    assert!(e.to_string().contains("phrase de passe incorrecte"), "{e}");
}

/// #341 : une lecture interrompue par une altération ne laisse pas de fichier partiel.
#[test]
fn opening_a_damaged_archive_leaves_no_partial_file() {
    let dir = tempfile::tempdir().unwrap();
    let sealed = sealed_of(dir.path(), &noise(2 * CHUNK + 5));
    let mut raw = std::fs::read(&sealed).unwrap();
    let last = raw.len() - 1;
    raw[last] ^= 1;
    std::fs::write(&sealed, &raw).unwrap();
    let out = dir.path().join("rendu");
    assert!(open(&sealed, &out, "phrase").is_err());
    assert!(!out.exists());
}

/// #341, mesure manuelle : une archive au-delà de l'ancien plafond de 512 Mo se chiffre
/// et se relit à l'identique, en flux.
/// `cargo test --release -p penelope-platform --lib over_the_old_cap -- --ignored`
#[test]
#[ignore = "mesure manuelle : écrit 1,2 Go sur le disque"]
fn an_archive_over_the_old_cap_round_trips() {
    use sha2::{Digest, Sha256};
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("gros");
    let len = 600 * CHUNK + 333;
    let mut expected = Sha256::new();
    {
        let mut f = std::io::BufWriter::new(std::fs::File::create(&src).unwrap());
        let block = noise(CHUNK);
        let mut left = len;
        while left > 0 {
            let n = left.min(CHUNK);
            f.write_all(&block[..n]).unwrap();
            expected.update(&block[..n]);
            left -= n;
        }
    }
    let sealed = dir.path().join("gros.enc");
    seal(&src, &sealed, "phrase").unwrap();
    std::fs::remove_file(&src).unwrap();
    assert!(std::fs::metadata(&sealed).unwrap().len() > 512 * 1024 * 1024);
    let mut got = Sha256::new();
    std::io::copy(&mut open_reader(&sealed, "phrase").unwrap(), &mut got).unwrap();
    assert_eq!(got.finalize(), expected.finalize());
}

use super::*;

/// #95 : la valeur ne figure qu'en hexadécimal, sur l'entrée standard ; guillemets,
/// barres obliques, espaces et `$` n'ont rien à échapper.
#[test]
fn a_secret_goes_through_stdin_in_hex() {
    let value = "a \"b\" \\c $HOME é";
    let line = add_command("penelope", "penelope.essai", value);
    assert!(!line.contains(value) && !line.contains("HOME"), "{line}");
    let hex = line
        .trim_end()
        .rsplit(' ')
        .next()
        .expect("hexadécimal en fin de commande")
        .to_string();
    assert_eq!(
        line,
        format!("add-generic-password -a penelope -s penelope.essai -U -X {hex}\n")
    );
    let back: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect();
    assert_eq!(String::from_utf8(back).unwrap(), value);
}

/// #95, sur la machine : écrit puis relit un secret d'essai dans le Trousseau de
/// session, et vérifie qu'aucun processus ne l'a eu en argument. Touche au Trousseau
/// du propriétaire : lancé à la main seulement.
#[test]
#[ignore]
fn a_secret_round_trips_through_the_keychain() {
    let dir = tempfile::tempdir().unwrap();
    let k = KeychainStore::new(dir.path());
    let value = "a \"b\" \\c $HOME fin";
    k.set("essai-issue-95", value).unwrap();
    assert_eq!(k.get("essai-issue-95").unwrap().as_deref(), Some(value));
    k.delete("essai-issue-95").unwrap();
}

/// #90 : le profil passe en argument ; aucun fichier n'est écrit, même après cent
/// enveloppes.
#[test]
fn the_profile_goes_inline_and_no_file_is_written() {
    let dir = std::env::temp_dir().join("penelope-sandbox");
    let count = || std::fs::read_dir(&dir).map(|r| r.count()).unwrap_or(0);
    let before = count();
    let profile = Profile::workspace_write("/tmp/ws");
    let mut last = None;
    for _ in 0..100 {
        last = Some(sandbox_wrapper(&profile, Path::new("/bin/echo"), &["x".into()]).unwrap());
    }
    let w = last.unwrap();
    if Path::new("/usr/bin/sandbox-exec").is_file() {
        assert_eq!(w.program, PathBuf::from("/usr/bin/sandbox-exec"));
        assert_eq!(w.args[0], "-p");
        assert!(w.args[1].starts_with("(version 1)"), "{}", w.args[1]);
        assert_eq!(&w.args[2..], ["/bin/echo", "x"]);
    }
    assert_eq!(count(), before, "aucun fichier de profil");
}

/// #89 et #90, sur la machine : un processus confiné ne lit pas un chemin refusé,
/// lit le reste, et le profil ne dépend d'aucun fichier qu'un voisin pourrait
/// réécrire. Lancé à la main : `cargo test -p penelope-platform seatbelt -- --ignored`.
#[test]
#[ignore]
fn seatbelt_enforces_denied_reads_on_this_mac() {
    let dir = tempfile::tempdir().unwrap();
    let secret_dir = dir.path().join("cles");
    std::fs::create_dir_all(&secret_dir).unwrap();
    std::fs::write(secret_dir.join("id_ed25519"), "CLE PRIVEE").unwrap();
    std::fs::write(dir.path().join("public.txt"), "lisible").unwrap();
    let mut profile = Profile::mcp_stdio(dir.path().join("data"), Vec::new());
    profile.deny_read = vec![secret_dir.clone()];

    let run = |file: &Path| {
        let w = sandbox_wrapper(
            &profile,
            Path::new("/bin/cat"),
            &[file.to_string_lossy().to_string()],
        )
        .unwrap();
        std::process::Command::new(&w.program)
            .args(&w.args)
            .output()
            .unwrap()
    };
    let denied = run(&secret_dir.join("id_ed25519"));
    assert!(!denied.status.success());
    assert!(
        String::from_utf8_lossy(&denied.stderr).contains("Operation not permitted"),
        "{}",
        String::from_utf8_lossy(&denied.stderr)
    );
    let allowed = run(&dir.path().join("public.txt"));
    assert_eq!(String::from_utf8_lossy(&allowed.stdout), "lisible");
}

/// #122, sur la machine : sous un profil qui ferme le trousseau, même un certificat
/// public du système est « introuvable » (c'est ce qui trompait le propriétaire) ; le
/// même profil déclaré avec le trousseau le trouve. Seul le trousseau des racines du
/// système est lu, jamais celui de l'utilisateur. Lancé à la main.
#[test]
#[ignore]
fn seatbelt_opens_the_keychain_only_when_declared_on_this_mac() {
    let dir = tempfile::tempdir().unwrap();
    let find = |profile: &Profile| {
        let w = sandbox_wrapper(
            profile,
            Path::new("/usr/bin/security"),
            &[
                "find-certificate".into(),
                "-c".into(),
                "Apple Root CA".into(),
                "/System/Library/Keychains/SystemRootCertificates.keychain".into(),
            ],
        )
        .unwrap();
        std::process::Command::new(&w.program)
            .args(&w.args)
            .output()
            .unwrap()
    };
    let closed = Profile::mcp_stdio(dir.path().join("data"), Vec::new());
    let refused = find(&closed);
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("could not be found"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    let found = find(&closed.clone().with_keychain(true));
    assert!(
        found.status.success(),
        "{}",
        String::from_utf8_lossy(&found.stderr)
    );
}

/// #91, sur la machine : sous `mcp-stdio` (réseau ouvert), une socket Unix locale
/// n'est pas joignable ; la résolution de nom l'est. Lancé à la main.
#[test]
#[ignore]
fn seatbelt_closes_unix_sockets_on_this_mac() {
    let dir = tempfile::Builder::new()
        .prefix("pnl")
        .tempdir_in("/tmp")
        .unwrap();
    let sock = dir.path().join("s.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    let profile = Profile::mcp_stdio(dir.path().join("data"), Vec::new());
    let python = |code: String| {
        let w = sandbox_wrapper(
            &profile,
            Path::new("/usr/bin/python3"),
            &["-c".into(), code],
        )
        .unwrap();
        std::process::Command::new(&w.program)
            .args(&w.args)
            .output()
            .unwrap()
    };
    let connect = python(format!(
        "import socket;s=socket.socket(socket.AF_UNIX);s.connect({:?})",
        sock.to_string_lossy()
    ));
    assert!(!connect.status.success());
    assert!(
        String::from_utf8_lossy(&connect.stderr).contains("Operation not permitted"),
        "{}",
        String::from_utf8_lossy(&connect.stderr)
    );
    let dns = python("import socket;socket.getaddrinfo('localhost', 80)".into());
    assert!(
        dns.status.success(),
        "{}",
        String::from_utf8_lossy(&dns.stderr)
    );
}

/// #148 : le 20/09, `security` a recopié des morceaux de la ligne reçue — donc du
/// secret — dans sa sortie d'erreur, et cette sortie est partie sur Telegram. Ici un
/// faux `security` fait exactement cela : le message rendu ne doit rien en garder.
#[test]
fn a_failing_security_never_leaks_the_value_into_the_error() {
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("faux-security");
    std::fs::write(
        &fake,
        "#!/bin/sh\ncat >&2\necho 'security: unknown command' >&2\nexit 1\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let store = KeychainStore::with_program(dir.path(), &fake);
    let value = "jeton-tres-secret-0123456789";
    let err = store.set("essai", value).unwrap_err().to_string();

    assert!(!err.contains(value), "valeur en clair : {err}");
    let hex: String = value.bytes().map(|b| format!("{b:02x}")).collect();
    for n in (8..=hex.len()).step_by(8) {
        assert!(
            !err.contains(&hex[..n]),
            "fragment hexadécimal de {n} caractères : {err}"
        );
    }
    assert!(!err.contains("unknown command"), "sortie recopiée : {err}");
    assert!(err.contains("Trousseau"), "{err}");
}

/// Un faux `security` **fidèle** : il range les items dans un dossier, et surtout il
/// rend `-w` comme le vrai — en clair si le mot de passe est imprimable, **en
/// hexadécimal sinon** (issue #157).
///
/// C'est toute la leçon de ce lot : le faux `security` de #148 rendait la valeur telle
/// quelle, donc la suite était verte pendant qu'un `Grant` de 8 Ko était relu faux sur
/// la machine. Un double de test qui ment sur le point qui compte ne prouve rien.
#[cfg(unix)]
fn faithful_security(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let store = dir.join("items");
    std::fs::create_dir_all(&store).unwrap();
    let fake = dir.join("faux-security");
    // `-i` lit la ligne `add-generic-password … -X <hex>` sur l'entrée standard.
    let script = format!(
        r#"#!/usr/bin/env python3
import os, re, sys
STORE = {store:?}
def path(service):
    return os.path.join(STORE, service.replace("/", "_"))
args = sys.argv[1:]
if args[:1] == ["-i"]:
    args = sys.stdin.readline().split()
cmd = args[0] if args else ""
def opt(flag):
    return args[args.index(flag) + 1] if flag in args else ""
service = opt("-s")
if cmd == "add-generic-password":
    open(path(service), "wb").write(bytes.fromhex(opt("-X")))
    sys.exit(0)
if cmd == "find-generic-password":
    try:
        raw = open(path(service), "rb").read()
    except FileNotFoundError:
        sys.stderr.write("could not be found\n"); sys.exit(44)
    # Le vrai `security` : en clair si imprimable, sinon en hexadécimal.
    try:
        text = raw.decode("utf-8")
        printable = all(c == "\n" or c == "\t" or ord(c) >= 32 for c in text)
    except UnicodeDecodeError:
        printable = False
    sys.stdout.write(text if printable else raw.hex())
    sys.exit(0)
if cmd == "delete-generic-password":
    try:
        os.remove(path(service)); sys.exit(0)
    except FileNotFoundError:
        sys.exit(44)
sys.exit(1)
"#,
        store = store.to_string_lossy()
    );
    std::fs::write(&fake, script).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    fake
}

/// #157 : un secret assez long pour être découpé était écrit juste et relu faux. La
/// tête portait un caractère de contrôle, `security -w` la rendait en hexadécimal, et
/// `get` rendait ces 42 caractères comme valeur du secret.
#[cfg(unix)]
#[test]
fn a_chunked_secret_survives_a_security_that_prints_hex() {
    let dir = tempfile::tempdir().unwrap();
    let fake = faithful_security(dir.path());
    let store = KeychainStore::with_program(dir.path(), &fake);

    let value = format!("{{\"access_token\":\"{}\"}}", "e".repeat(8 * 1024));
    store.set("grant", &value).expect("écriture");
    let back = store.get("grant").expect("relecture");
    let relus = back.as_deref().map(str::len).unwrap_or(0);
    assert_eq!(
        back.as_deref(),
        Some(value.as_str()),
        "{} octets écrits, {relus} relus",
        value.len()
    );

    // La tête écrite est bien imprimable : c'est ce qui la fait revenir intacte.
    let head = store.read_item(&store.service("grant")).unwrap();
    assert_eq!(head, chunks::header(chunks::count(&head).unwrap()));
    assert!(
        head.chars().all(|c| !c.is_control()),
        "aucun caractère de contrôle dans la tête : {head:?}"
    );

    // Un secret court garde sa forme d'avant.
    store.set("court", "sk-abc").unwrap();
    assert_eq!(store.get("court").unwrap().as_deref(), Some("sk-abc"));
    store.delete("grant").unwrap();
    assert_eq!(store.get("grant").unwrap(), None);
}

/// #157 : un secret posé par une version antérieure porte l'ancienne marque, avec son
/// caractère de contrôle. Il doit rester lisible après la mise à jour, sans migration.
#[cfg(unix)]
#[test]
fn a_secret_written_with_the_old_mark_is_still_read() {
    let dir = tempfile::tempdir().unwrap();
    let fake = faithful_security(dir.path());
    let store = KeychainStore::with_program(dir.path(), &fake);

    // Écrit à la main comme le faisait 0.17.35 : morceaux, puis tête marquée `\x01`.
    store
        .write_item(&store.chunk_service("ancien", 0), "début-")
        .unwrap();
    store
        .write_item(&store.chunk_service("ancien", 1), "fin")
        .unwrap();
    let old_head = format!("{}{}", chunks::LEGACY_MARK, 2);
    store
        .write_item(&store.service("ancien"), &old_head)
        .unwrap();

    // Le faux `security` la rend en hexadécimal, comme le vrai.
    assert_eq!(
        store.get("ancien").unwrap().as_deref(),
        Some("début-fin"),
        "l'ancienne marque reste lisible"
    );
}

/// #157 : une tête illisible alors que des morceaux existent ne doit **jamais** passer
/// pour la valeur. Mieux vaut une erreur qu'un jeton faux qui rendra 401 plus tard.
#[cfg(unix)]
#[test]
fn an_unreadable_head_is_an_error_not_a_value() {
    let dir = tempfile::tempdir().unwrap();
    let fake = faithful_security(dir.path());
    let store = KeychainStore::with_program(dir.path(), &fake);

    store
        .write_item(&store.chunk_service("casse", 0), "morceau")
        .unwrap();
    store
        .write_item(&store.service("casse"), "tête-abîmée")
        .unwrap();
    let err = store.get("casse").unwrap_err().to_string();
    assert!(err.contains("en-tête de morceaux illisible"), "{err}");

    // Sans morceau, la même tête est un secret ordinaire d'une version antérieure.
    store
        .write_item(&store.service("simple"), "tête-abîmée")
        .unwrap();
    assert_eq!(store.get("simple").unwrap().as_deref(), Some("tête-abîmée"));
}

/// #148 : le vrai Trousseau, avec un secret de 16 Ko. Écrit un item réel, donc
/// `#[ignore]` : `cargo test -p penelope-platform -- --ignored keychain`.
#[test]
#[ignore = "écrit dans le Trousseau de l'utilisateur"]
fn keychain_holds_a_long_secret_and_forgets_it() {
    if !KeychainStore::available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let store = KeychainStore::new(dir.path());
    let name = "penelope.test.gros-secret";
    let value = format!(
        "{{\"access_token\":\"{}\",\"refresh_token\":\"{}\"}}",
        "e".repeat(8 * 1024),
        "r".repeat(8 * 1024)
    );

    store.set(name, &value).expect("écriture");
    assert_eq!(store.get(name).unwrap().as_deref(), Some(value.as_str()));
    assert_eq!(store.list().unwrap(), [name], "un seul nom logique");

    // Réécriture plus courte : les morceaux de la version longue ne survivent pas.
    store.set(name, "court").expect("réécriture");
    assert_eq!(store.get(name).unwrap().as_deref(), Some("court"));
    assert!(
        store.read_item(&store.chunk_service(name, 0)).is_none(),
        "morceau resté derrière"
    );

    store.delete(name).expect("suppression");
    assert_eq!(store.get(name).unwrap(), None);
    assert!(store.list().unwrap().is_empty());
}

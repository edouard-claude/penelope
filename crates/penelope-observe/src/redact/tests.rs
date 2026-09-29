//! Tests du rédacteur. Chacun travaille sur un `Redactor` neuf : ni les secrets ni les
//! valeurs apprises des tests voisins ne le touchent (issue #258).

use super::*;
use serde_json::json;

/// #153 : une référence `${SECRET:` **sans** accolade fermante ne doit pas faire
/// boucler la rédaction. Six lignes de ce genre, écrites par Pénélope en expliquant
/// la syntaxe, ont fait déborder la pile du thread écrivain au démarrage et revenir
/// en arrière les versions 0.17.30 et 0.17.31.
///
/// Le test ne peut pas observer l'ancien comportement : un débordement de pile abat
/// le processus (`abort`), il ne se rattrape pas. Il vérifie donc que la rédaction
/// **rend**, et que les références complètes traversent toujours intactes.
#[test]
fn an_orphan_secret_reference_never_loops() {
    let r = Redactor::default();
    for orphan in [
        "secrets via ${SECRET:…}",
        "${SECRET:timeperformance</pre>",
        "<code>${SECRET:…}</code> supporté dans",
        "${SECRET:",
        "${SECRET:nom sans accolade",
        "${SECRET:a} puis ${SECRET:",
    ] {
        let out = r.redact(orphan);
        assert!(!out.is_empty(), "« {orphan} » doit rendre un texte");
    }
    // Ce que #37 garantit ne bouge pas : une référence complète traverse intacte,
    // même à côté d'une orpheline.
    // Une orpheline est un texte comme un autre : ni référence, ni secret à masquer
    // quand rien derrière elle n'en a la forme.
    assert_eq!(
        r.redact("${SECRET:telegram_bot_token} et ${SECRET:"),
        "${SECRET:telegram_bot_token} et ${SECRET:"
    );
    assert_eq!(
        r.redact("avant ${SECRET:a} milieu ${SECRET:b} après"),
        "avant ${SECRET:a} milieu ${SECRET:b} après"
    );
}

#[test]
fn masks_bearer_and_jwt() {
    let r = Redactor::default();
    let s = "Authorization: Bearer abcdefghijklmnop1234";
    assert!(!r.redact(s).contains("abcdefghijklmnop"));
    let jwt = "eyJhbGciOi.eyJzdWIiOjE.SflKxwRJSMeKK";
    assert_eq!(r.redact(jwt), MASK);
}

#[test]
fn masks_provider_keys() {
    let r = Redactor::default();
    for k in [
        "sk-or-v1-0123456789abcdef0123456789abcdef",
        "ghp_0123456789abcdef0123456789abcdef",
        "AKIAIOSFODNN7EXAMPLE",
        "xoxb-1234567890-abcdefghij",
    ] {
        let out = r.redact(&format!("clé = {k} fin"));
        assert!(!out.contains(k), "non masqué : {k} → {out}");
    }
}

#[test]
fn masks_telegram_bot_token() {
    let r = Redactor::default();
    let t = "123456789:AAH-abcdefghijklmnopqrstuvwxyz012345";
    assert!(!r.redact(t).contains("AAH-"));
}

#[test]
fn card_number_uses_luhn() {
    let r = Redactor::default();
    // Numéro de test Visa valide au sens de Luhn.
    assert_eq!(
        r.redact("carte 4111 1111 1111 1111 fin"),
        format!("carte {MASK} fin")
    );
    // Une suite de chiffres qui ne passe pas Luhn n'est pas masquée.
    let ident = "1234567890123456";
    assert!(!luhn(ident));
    assert!(r.redact(&format!("ref {ident}")).contains(ident));
}

/// #148 : le `Grant` Codex est parti en clair parce qu'il était **en hexadécimal** :
/// deux familles de caractères seulement, donc invisible aux règles d'entropie. Une
/// empreinte SHA-256, elle, reste lisible.
#[test]
fn a_long_hex_run_is_masked_but_a_sha256_digest_is_not() {
    let r = Redactor::default();
    let grant = "7b22616363657373".repeat(200);
    assert!(grant.len() > 3_000);
    let red = r.redact(&format!("security: unknown command \"{grant}"));
    assert!(
        !red.contains("7b22616363657373"),
        "hexadécimal en clair : {red}"
    );
    assert!(red.contains(MASK), "{red}");

    // 64 caractères exactement : une empreinte, on la garde.
    let digest = "a3f1".repeat(16);
    assert_eq!(digest.len(), 64);
    let line = format!("skill revue-de-code body_hash {digest}");
    assert_eq!(r.redact(&line), line, "une empreinte reste lisible");

    // 65 et plus : ce n'est plus une empreinte.
    let long = format!("{digest}b");
    assert!(r.redact(&long).contains(MASK), "{long}");

    // Ce qui n'est pas de l'hexadécimal n'est pas concerné par cette règle.
    let path = "/Users/edouard/Library/Application-Support/Penelope/secret-names.json";
    assert_eq!(r.redact(path), path);
}

#[test]
fn registered_secret_is_masked_even_without_pattern() {
    let r = Redactor::default();
    r.register("motdepasseordinaire");
    let out = r.redact("le mot est motdepasseordinaire ici");
    assert!(!out.contains("motdepasseordinaire"), "{out}");
}

/// #258 : ce test comparait le compte du rédacteur du processus avant et après, pendant
/// que les tests voisins enregistraient et oubliaient leurs secrets ; il a échoué en CI
/// le 29/09. Sur un rédacteur neuf, le compte ne dépend plus que de lui.
#[test]
fn short_values_are_not_registered() {
    let r = Redactor::default();
    r.register("abc");
    assert_eq!(r.registered_count(), 0);
    r.register("abcdef");
    assert_eq!(r.registered_count(), 1);
}

/// #258 : deux rédacteurs ne partagent rien ; celui du processus ne voit pas ce qu'un
/// test enregistre ou apprend sur le sien.
#[test]
fn a_redactor_does_not_see_what_another_one_learned() {
    let a = Redactor::default();
    let b = Redactor::default();
    a.register("valeur-enregistree-258");
    a.learn_secrets("KEY = \"cle258ab34ef56gh78\"");
    let line = "valeur-enregistree-258 et cle258ab34ef56gh78";
    assert!(!a.redact(line).contains("258"), "{}", a.redact(line));
    assert_eq!(b.redact(line), line);
    assert_eq!(redact(line), line, "le rédacteur du processus n'a rien vu");
}

/// #258 : après `key=`, `token =`, `Bearer` ou `Basic`, un mot ordinaire n'est pas un
/// secret à retenir. Il était appris puis masqué partout, pour tout le processus : le
/// lot #192 a dû réécrire une consigne où `project` devenait « [secret masqué] ».
#[test]
fn an_ordinary_word_after_a_keyword_is_not_learned() {
    let r = Redactor::default();
    r.learn_secrets("session_metadata op=`set` key=`project` entry=`{}`");
    r.learn_secrets("api_key = os.environ[\"K\"]\ntoken = self.token\nHTTP Basic authentication");
    r.learn_secrets("key=project_name, password: 12345678, token=v0.17.10");
    for kept in [
        "clé `project`",
        "x = os.environ[\"A\"]",
        "self.token",
        "la authentication",
        "project_name",
        "12345678",
        "v0.17.10",
    ] {
        assert_eq!(r.redact(kept), kept, "{kept}");
    }
    // Ce qui marchait : un jeton lu après un mot-clé, recopié seul, est masqué (#134),
    // qu'il vienne d'une affectation ou d'un en-tête d'autorisation.
    r.learn_secrets(
        "KEY = \"dev-secret-aaaa1111\"\nAuthorization: Bearer eyJhbGciOi.eyJzdWIiOjE.Sfl0KxwR",
    );
    for secret in ["dev-secret-aaaa1111", "eyJhbGciOi.eyJzdWIiOjE.Sfl0KxwR"] {
        assert!(
            !r.redact(&format!("copie : {secret}")).contains(secret),
            "{secret}"
        );
    }
    // Et l'affectation elle-même reste masquée sur place, accent grave ou non.
    for line in ["password = `Hunter2Hunter2`", "password: Hunter2Hunter2"] {
        assert!(!r.redact(line).contains("Hunter2"), "{}", r.redact(line));
    }
}

#[test]
fn redaction_is_idempotent() {
    let r = Redactor::default();
    let s = "Bearer abcdefghijklmnop1234";
    let a = r.redact(s);
    assert_eq!(r.redact(&a), a);
}

#[test]
fn json_sensitive_keys_are_masked() {
    let r = Redactor::default();
    let v = json!({"api_key":"quelquechose","nested":{"token":"xyz"},"ok":"visible"});
    let r = r.redact_json(&v);
    assert_eq!(r["api_key"], MASK);
    assert_eq!(r["nested"]["token"], MASK);
    assert_eq!(r["ok"], "visible");
}

/// #134 : une clé sans préfixe connu, recopiée d'un fichier dans une commande, est
/// masquée quand elle est stockée ou journalisée : par son affectation (`KEY = "…"`,
/// `'x-api-key': '…'`, `"password": "…"`), par sa forme (jeton long et aléatoire), ou
/// parce qu'elle a été lue plus tôt. Un chemin, une URL, un hachage, un identifiant
/// lisible restent intacts.
#[test]
fn a_key_copied_from_a_file_is_masked_where_it_is_stored() {
    let r = Redactor::default();
    let key = "Zx9kQ2mV7pLr4TbW1nHs8YcD3fGa6JuE0oIq5*RtKyNw2BvXe7LmPz4SdHj1Ua";
    let command = format!("python3 - <<'EOF'\nKEY = \"{key}\"\nprint(1)\nEOF");
    assert!(!r.redact(&command).contains(key), "{}", r.redact(&command));
    let js = "headers: { 'x-api-key': 'dev-secret-aaaa1111' }";
    assert!(
        !r.redact(js).contains("dev-secret-aaaa1111"),
        "{}",
        r.redact(js)
    );
    let py = r#"login({"email": "a@b.fr", "password": "Motdepasse2026"})"#;
    assert!(!r.redact(py).contains("Motdepasse2026"), "{}", r.redact(py));
    // Une valeur lue plus tôt, recopiée seule, est masquée aussi.
    r.learn_secrets("const cfg = { apiKey: 'ab12cd34ef56gh78' };");
    assert!(
        !r.redact(r#"{"p": "ab12cd34ef56gh78"}"#)
            .contains("ab12cd34ef56gh78")
    );
    for kept in [
        "https://github.com/edouard-claude/penelope/releases/tag/v0.17.10",
        "/Users/essai/Code/agent/penelope/crates/penelope-daemon/src/executor.rs",
        "a_failing_command_of_41_lines_is_returned_whole_and_more",
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        "s_01M2TAQFBTN50F030S73RWZFME",
    ] {
        assert_eq!(r.redact(kept), kept, "{kept}");
    }
}

/// #132 : un nombre collé à un identifiant n'est pas une carte pour le filtre
/// d'écriture ; une vraie carte, isolée ou nommée, l'est toujours ; les journaux
/// masquent toujours tout nombre qui passe Luhn et la longueur (#26).
#[test]
fn a_number_inside_an_identifier_is_not_a_card() {
    let r = Redactor::default();
    let id = "command-output:38228-1743576040856618";
    assert!(luhn("1743576040856618"), "le cas vécu passe bien Luhn");
    assert!(!r.contains_secret(id), "{id}");
    assert_eq!(r.secret_kind(id), None);
    assert_eq!(r.secret_kind("id=4539148803436467"), None);
    assert_eq!(r.secret_kind("run/4539148803436467/log"), None);
    for card in [
        "4539 1488 0343 6467",
        "ma carte 4539148803436467.",
        "carte 4539148803436467",
        "carte:4539148803436467",
        "cb=4539-1488-0343-6467",
    ] {
        assert_eq!(r.secret_kind(card), Some("numéro de carte"), "{card}");
    }
    assert_eq!(
        r.secret_fragment("ma carte 4539 1488 0343 6467").as_deref(),
        Some("…6467")
    );
    assert_eq!(
        r.secret_fragment("clé sk-0123456789abcdefgh").as_deref(),
        Some("sk-0…")
    );
    assert!(r.redact(id).contains(MASK), "journaux : masqué quand même");
}

#[test]
fn contains_secret_detects_card_and_key() {
    let r = Redactor::default();
    assert!(r.contains_secret("ma carte 4111111111111111"));
    assert!(r.contains_secret("sk-0123456789abcdefgh"));
    assert!(!r.contains_secret("une phrase tout à fait banale"));
    assert_eq!(
        r.secret_kind("ma carte 4111111111111111"),
        Some("numéro de carte")
    );
}

/// CA 13 : un secret planté n'apparaît dans aucun log, événement ou message.
#[test]
fn ca_13_2_planted_secret_never_leaks() {
    let r = Redactor::default();
    let secret = "sk-or-v1-deadbeefdeadbeefdeadbeefdeadbeef";
    r.register(secret);
    let event = json!({
        "tool": "http_fetch",
        "args": {"headers": {"Authorization": format!("Bearer {secret}")}},
        "log": format!("appel avec {secret}"),
    });
    let out = serde_json::to_string(&r.redact_json(&event)).unwrap();
    assert!(!out.contains("deadbeef"), "{out}");
}

#[test]
fn secret_spans_point_at_values_only() {
    let r = Redactor::default();
    let t = "Stripe de test : sk_test_FauxCle0123456, et password: Hunter2Hunter2 ; \
             jeton ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.";
    let spans = r.secret_spans(t);
    let values: Vec<&str> = spans.iter().map(|s| &t[s.start..s.end]).collect();
    assert_eq!(
        values,
        vec![
            "sk_test_FauxCle0123456",
            "Hunter2Hunter2",
            "ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ]
    );
    assert_eq!(spans[0].kind, "clé stripe");
    // Clé OpenRouter : deux motifs, une seule valeur.
    let k = "clé sk-or-v1-0123456789abcdef0123456789";
    assert_eq!(r.secret_spans(k).len(), 1);
    assert!(r.secret_spans("carte 4111 1111 1111 1111").is_empty());
    assert!(r.secret_spans("client cus_NffrFeUfNV2Hib").is_empty());
    // Une référence à un secret rangé n'est pas un secret, et survit à la redaction.
    let t = "clé Stripe : ${SECRET:cle-stripe-test-1f2e3d4c}, password: Hunter2Hunter2";
    assert_eq!(r.secret_kind("${SECRET:cle-stripe-test-1f2e3d4c}"), None);
    assert!(!r.contains_secret("${SECRET:cle-stripe-test-1f2e3d4c}"));
    let spans = r.secret_spans(t);
    assert_eq!(spans.len(), 1);
    assert_eq!(&t[spans[0].start..spans[0].end], "Hunter2Hunter2");
    assert_eq!(
        r.redact(t),
        format!("clé Stripe : ${{SECRET:cle-stripe-test-1f2e3d4c}}, {MASK}")
    );
}

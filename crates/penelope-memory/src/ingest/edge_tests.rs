//! Cas limites de l'ingestion : OCR, entités, balisage tronqué, noms de fichiers.

use super::*;

/// Le texte OCR est normalisé, borné, et un OCR vide est une erreur qui le dit.
#[test]
fn ocr_text_is_normalised_bounded_and_never_empty() {
    let e = from_ocr("Facture\r\n\r\n\r\n\r\nTotal : 12 €", 2).unwrap();
    assert_eq!(e.format, "pdf (OCR)");
    assert_eq!(e.pages, Some(2));
    assert!(!e.truncated);
    assert!(e.text.contains("Facture"), "{}", e.text);
    assert_eq!(
        from_ocr("   \n  ", 1).unwrap_err(),
        "aucun texte reconnu dans ce PDF, même par OCR"
    );
    let long = "a".repeat(MAX_TEXT_CHARS + 10);
    let e = from_ocr(&long, 1).unwrap();
    assert!(e.truncated);
    assert_eq!(e.text.chars().count(), MAX_TEXT_CHARS);
}

/// Un texte trop long est coupé et le dit ; un fichier sans extension est refusé en le
/// nommant ; un texte vide n'est pas un document.
#[test]
fn extraction_bounds_and_refusals() {
    let long = "b".repeat(MAX_TEXT_CHARS + 1);
    let e = extract("long.txt", long.as_bytes()).unwrap();
    assert!(e.truncated);
    let err = extract("LISEZMOI", b"texte").unwrap_err();
    assert!(err.contains("format `sans extension` non ingéré"), "{err}");
    assert_eq!(
        extract("vide.md", b"  \n ").unwrap_err(),
        "le document ne contient aucun texte"
    );
}

/// Entités nommées et numériques décodées ; une esperluette seule ou une entité inconnue
/// restent telles quelles.
#[test]
fn entities_are_decoded_and_unknown_ones_kept() {
    assert_eq!(
        decode_entities("a &amp; b &lt;c&gt; &quot;d&quot; &apos;e&apos;&nbsp;f"),
        "a & b <c> \"d\" 'e' f"
    );
    assert_eq!(decode_entities("&#65;&#x42;&#X43;"), "ABC");
    assert_eq!(
        decode_entities("R&D sans point-virgule"),
        "R&D sans point-virgule"
    );
    assert_eq!(decode_entities("&inconnu; &#xZZ;"), "&inconnu; &#xZZ;");
}

/// #452 : la fenêtre de 12 octets où chercher le `;` coupait au milieu d'un caractère
/// accentué et la tranche paniquait. « R&D à Montréal » suffisait.
#[test]
fn an_ampersand_before_accented_text_is_kept() {
    for text in [
        "R&D à Montréal",
        "Marks & Spencer à Montréal, Québec",
        "&ééééééé",
        "&aaaaaaaaaa€;",
        "fin &",
        "😀&😀😀😀😀;",
    ] {
        assert_eq!(decode_entities(text), text);
    }
    assert_eq!(
        decode_entities(
            "caf&eacute; &agrave; No&euml;l &laquo;&nbsp;&OElig;uvre&nbsp;&raquo; 12&nbsp;&euro;&hellip;"
        ),
        "café à Noël « Œuvre » 12 €…"
    );
    assert_eq!(decode_entities("&#233;&#xE9;&#x1F600;"), "éé😀");
    let page = html_to_text("<p>R&D à Montréal</p><p>Budget&nbsp;: 3&nbsp;M&euro;</p>");
    let lines: Vec<&str> = page.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines, ["R&D à Montréal", "Budget : 3 M€"]);
}

/// #452 : aucune suite de caractères ne fait paniquer le décodage. Toutes les chaînes
/// de cinq signes au plus sur un alphabet qui mêle les marques d'entité et des
/// caractères d'un à quatre octets, puis une esperluette devant chaque décalage d'une
/// queue multi-octets. Sans `;`, rien n'est décodé.
#[test]
fn no_string_makes_the_decoding_panic() {
    const ALPHABET: [char; 9] = ['&', ';', '#', 'x', '4', 'a', 'é', '€', '😀'];
    let mut count = 0usize;
    let mut stack: Vec<String> = vec![String::new()];
    while let Some(s) = stack.pop() {
        let out = decode_entities(&s);
        if !s.contains(';') {
            assert_eq!(out, s);
        }
        count += 1;
        if s.chars().count() < 5 {
            stack.extend(ALPHABET.iter().map(|c| format!("{s}{c}")));
        }
    }
    assert_eq!(count, (0..=5).map(|n| 9usize.pow(n)).sum::<usize>());
    for pad in 0..16 {
        for tail in ["é€😀é€😀é€😀", "😀😀😀😀;", "ééééééééé;"] {
            let s = format!("&{}{tail}", "a".repeat(pad));
            assert_eq!(decode_entities(&s), s);
        }
    }
}

/// #452 : une panique de la lecture est rendue comme une erreur qui nomme le document,
/// pas comme une tâche perdue.
#[test]
fn a_panic_during_extraction_names_the_document() {
    let err = guarded("rapport 2026.docx", || panic!("tranche")).unwrap_err();
    assert!(
        err.contains("`rapport 2026.docx`") && err.contains("contenu inattendu"),
        "{err}"
    );
    let page = extract("note.html", "<p>R&D à Montréal</p>".as_bytes()).unwrap();
    assert_eq!(page.text, "R&D à Montréal");
}

/// Du HTML tronqué (commentaire ou balise jamais fermés) ne fait rien perdre avant la
/// coupure, et le contenu des scripts n'entre pas. Un commentaire jamais fermé est
/// retiré jusqu'à la fin.
#[test]
fn truncated_html_keeps_what_came_before() {
    let t = html_to_text("<p>Bonjour</p><!-- jamais fermé");
    assert_eq!(t.trim(), "Bonjour");
    let t = html_to_text("<p>Salut</p><div");
    assert!(t.contains("Salut"), "{t}");
    let t = html_to_text("<script>var x = 1;</script><p>Texte</p>");
    assert!(!t.contains("var x"), "{t}");
    assert!(t.contains("Texte"), "{t}");
    let t = docx_xml_to_text("<w:p><w:t>Un</w:t></w:p><w:p");
    assert!(t.contains("Un"), "{t}");
}

/// #223 : un `<` isolé dans du texte brut (comparaison, flèche, code) n'est pas une
/// balise ; le texte qui le précède n'est pas doublé.
#[test]
fn a_lone_less_than_is_kept_once() {
    assert_eq!(html_to_text("a < b"), "a < b");
    assert_eq!(html_to_text("x <- y"), "x <- y");
    assert_eq!(html_to_text("<p>Un</p>2 < 3"), "\nUn\n2 < 3");
    assert_eq!(
        html_to_text("<p>Bonjour</p><!-- jamais fermé").trim(),
        "Bonjour"
    );
}

/// Le nom d'un document devient un slug sans accents ni ponctuation.
#[test]
fn file_names_become_accentless_slugs() {
    assert_eq!(
        slugify("Déclaration à l'œuvre (2026).pdf"),
        "declaration-a-l-oeuvre-2026"
    );
    assert_eq!(slugify("Ça ÿ Ñ æ.txt"), "ca-y-n-ae");
}

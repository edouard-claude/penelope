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

/// Du HTML tronqué (commentaire ou balise jamais fermés) ne fait rien perdre avant la
/// coupure, et le contenu des scripts n'entre pas. (Le texte d'un commentaire jamais
/// fermé, lui, est gardé : relevé, non vérifié ici.)
#[test]
fn truncated_html_keeps_what_came_before() {
    let t = html_to_text("<p>Bonjour</p><!-- jamais fermé");
    assert!(t.contains("Bonjour"), "{t}");
    let t = html_to_text("<p>Salut</p><div");
    assert!(t.contains("Salut"), "{t}");
    let t = html_to_text("<script>var x = 1;</script><p>Texte</p>");
    assert!(!t.contains("var x"), "{t}");
    assert!(t.contains("Texte"), "{t}");
    let t = docx_xml_to_text("<w:p><w:t>Un</w:t></w:p><w:p");
    assert!(t.contains("Un"), "{t}");
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

use super::*;

/// Un PNG de 1 × 1, en base64.
const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

fn result(blocks: Value) -> Value {
    json!({"content": blocks, "isError": false})
}

/// #304 : une image est écrite sous un nom tiré de son contenu, la mention donne le
/// chemin, le base64 a disparu ; la même image rendue deux fois est le même fichier.
#[test]
fn an_image_block_is_written_and_only_its_path_remains() {
    let dir = tempfile::tempdir().unwrap();
    let mut v = result(json!([
        {"type": "text", "text": "capture jointe"},
        {"type": "image", "mimeType": "image/png", "data": PNG, "text": "[image …]"},
    ]));
    store(&mut v, dir.path());
    let b = &v["content"][1];
    let path = PathBuf::from(b[SAVED].as_str().unwrap());
    assert_eq!(path.parent().unwrap(), dir.path());
    assert_eq!(path.extension().unwrap(), "png");
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes[..4], b"\x89PNG");
    assert_eq!(b["bytes"], bytes.len());
    let text = b["text"].as_str().unwrap();
    assert!(
        text.contains("image image/png") && text.contains("enregistrée"),
        "{text}"
    );
    assert!(text.contains(&path.display().to_string()), "{text}");
    assert!(!v.to_string().contains(PNG), "aucun base64 ne reste : {v}");
    assert_eq!(v["content"][0]["text"], "capture jointe");

    let mut again = result(json!([{"type": "image", "mimeType": "image/png", "data": PNG}]));
    store(&mut again, dir.path());
    assert_eq!(again["content"][0][SAVED], b[SAVED], "nom stable");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    assert_eq!(saved_images(&v, &[dir.path().to_path_buf()]), vec![path]);
}

/// #304 : un audio est écrit de même, mais n'est pas une image à montrer au modèle.
#[test]
fn an_audio_block_is_written_but_not_shown() {
    let dir = tempfile::tempdir().unwrap();
    let data = base64::engine::general_purpose::STANDARD.encode(b"RIFF0000WAVEfmt ");
    let mut v = result(json!([{"type": "audio", "mimeType": "audio/wav", "data": data}]));
    store(&mut v, dir.path());
    let b = &v["content"][0];
    let path = PathBuf::from(b[SAVED].as_str().unwrap());
    assert_eq!(path.extension().unwrap(), "wav");
    assert_eq!(std::fs::read(&path).unwrap(), b"RIFF0000WAVEfmt ");
    let text = b["text"].as_str().unwrap();
    assert!(
        text.starts_with("[audio audio/wav, 1 Ko, enregistré : "),
        "{text}"
    );
    assert!(!v.to_string().contains(&data));
    assert!(saved_images(&v, &[dir.path().to_path_buf()]).is_empty());
}

/// #304 : au-delà du plafond, ou illisible, rien n'est écrit et la mention le dit.
#[test]
fn an_oversized_or_broken_block_is_not_written() {
    let dir = tempfile::tempdir().unwrap();
    let big = "A".repeat((MEDIA_MAX_BYTES / 3 + 8) * 4);
    let mut v = result(json!([
        {"type": "image", "mimeType": "image/png", "data": big},
        {"type": "image", "mimeType": "image/jpeg", "data": "pas du base64 !"},
    ]));
    store(&mut v, dir.path());
    let over = v["content"][0]["text"].as_str().unwrap();
    assert!(over.contains("au-delà du plafond de 10.0 Mo"), "{over}");
    assert!(over.ends_with("non enregistrée]"), "{over}");
    let broken = v["content"][1]["text"].as_str().unwrap();
    assert!(broken.contains("base64 illisible"), "{broken}");
    assert!(v["content"][0].get(SAVED).is_none() && v["content"][1].get(SAVED).is_none());
    assert!(!dir.path().exists() || std::fs::read_dir(dir.path()).unwrap().count() == 0);
    assert!(
        v.to_string().len() < 1_000,
        "le base64 refusé ne reste pas non plus"
    );
}

/// Un `saved` posé par le serveur est retiré, et un chemin hors des racines n'est pas
/// montré : le serveur ne choisit pas le fichier que voit le modèle.
#[test]
fn a_server_cannot_choose_the_file_shown() {
    let dir = tempfile::tempdir().unwrap();
    let mut v = result(json!([
        {"type": "image", SAVED: "/etc/secret.png", "text": "voir"},
    ]));
    store(&mut v, dir.path());
    assert!(v["content"][0].get(SAVED).is_none());
    let forged = result(json!([{"type": "image", SAVED: "/etc/secret.png"}]));
    assert!(saved_images(&forged, &[dir.path().to_path_buf()]).is_empty());
}

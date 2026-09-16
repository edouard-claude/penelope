//! Reconnaissance de texte des PDF scannés.
//!
//! macOS : chaque page est rendue par PDFKit puis lue par Vision
//! (`VNRecognizeTextRequest`, français et anglais). Le petit programme Swift qui le fait est
//! compilé une fois dans le cache (`swiftc`, outils de développement Xcode) ; son nom porte
//! le hash de sa source, une nouvelle version se recompile d'elle-même. Ailleurs : non
//! disponible.

use crate::{PlatformError, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

/// Source du programme d'OCR.
pub const OCR_SWIFT: &str = r#"import AppKit
import Foundation
import PDFKit
import Vision

let args = CommandLine.arguments
guard args.count >= 2, let doc = PDFDocument(url: URL(fileURLWithPath: args[1])) else {
    FileHandle.standardError.write("PDF illisible\n".data(using: .utf8)!)
    exit(2)
}
let maxPages = args.count >= 3 ? (Int(args[2]) ?? 50) : 50
let count = min(doc.pageCount, maxPages)
for index in 0..<count {
    guard let page = doc.page(at: index) else { continue }
    let bounds = page.bounds(for: .mediaBox)
    let scale: CGFloat = max(1.0, min(3.0, 2000.0 / max(bounds.width, bounds.height)))
    let image = page.thumbnail(
        of: NSSize(width: bounds.width * scale, height: bounds.height * scale), for: .mediaBox)
    var rect = NSRect(origin: .zero, size: image.size)
    guard let cg = image.cgImage(forProposedRect: &rect, context: nil, hints: nil) else {
        continue
    }
    let request = VNRecognizeTextRequest()
    request.recognitionLevel = .accurate
    request.usesLanguageCorrection = true
    request.recognitionLanguages = ["fr-FR", "en-US"]
    let handler = VNImageRequestHandler(cgImage: cg, options: [:])
    do {
        try handler.perform([request])
    } catch {
        continue
    }
    let lines = (request.results ?? []).compactMap { $0.topCandidates(1).first?.string }
    print("\u{0C}page \(index + 1)")
    print(lines.joined(separator: "\n"))
}
"#;

/// Texte reconnu.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrText {
    pub text: String,
    /// Pages lues.
    pub pages: usize,
}

/// Découpe la sortie du programme : un saut de page puis `page N` avant chaque page.
pub fn parse_output(raw: &str) -> OcrText {
    let mut pages = 0;
    let mut text = String::new();
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix('\u{0C}')
            && rest.starts_with("page ")
        {
            pages += 1;
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            continue;
        }
        text.push_str(line);
        text.push('\n');
    }
    OcrText {
        text: text.trim().to_string(),
        pages,
    }
}

/// Lit le texte d'un PDF scanné. `cache` accueille le programme compilé.
pub fn pdf_text(pdf: &Path, cache: &Path, max_pages: usize, timeout: Duration) -> Result<OcrText> {
    if !cfg!(target_os = "macos") {
        return Err(PlatformError::Unsupported(
            "OCR des PDF disponible sur macOS seulement".into(),
        ));
    }
    let helper = helper(cache)?;
    let out_path = cache.join(format!("ocr-{}.txt", std::process::id()));
    let out = std::fs::File::create(&out_path)?;
    let mut child = std::process::Command::new(&helper)
        .arg(pdf)
        .arg(max_pages.max(1).to_string())
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(Stdio::piped())
        .spawn()?;
    let status = wait_with_timeout(&mut child, timeout)?;
    let raw = std::fs::read_to_string(&out_path).unwrap_or_default();
    let _ = std::fs::remove_file(&out_path);
    if !status.success() {
        let mut err = String::new();
        if let Some(mut e) = child.stderr.take() {
            use std::io::Read;
            let _ = e.read_to_string(&mut err);
        }
        return Err(PlatformError::Process(format!(
            "OCR en échec : {}",
            err.trim()
        )));
    }
    Ok(parse_output(&raw))
}

fn helper(cache: &Path) -> Result<PathBuf> {
    let hash: String = Sha256::digest(OCR_SWIFT.as_bytes())[..6]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let dir = cache.join("ocr");
    std::fs::create_dir_all(&dir)?;
    let binary = dir.join(format!("penelope-ocr-{hash}"));
    if binary.is_file() {
        return Ok(binary);
    }
    let source = dir.join(format!("penelope-ocr-{hash}.swift"));
    std::fs::write(&source, OCR_SWIFT)?;
    let staged = dir.join(format!(".penelope-ocr-{hash}.{}", std::process::id()));
    let mut child = std::process::Command::new("/usr/bin/xcrun")
        .args(["swiftc", "-O"])
        .arg(&source)
        .arg("-o")
        .arg(&staged)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            PlatformError::Unsupported(format!(
                "OCR indisponible, compilateur Swift introuvable ({e}) : xcode-select --install"
            ))
        })?;
    let status = wait_with_timeout(&mut child, Duration::from_secs(300))?;
    if !status.success() || !staged.is_file() {
        let _ = std::fs::remove_file(&staged);
        return Err(PlatformError::Unsupported(
            "OCR indisponible : la compilation du lecteur Vision a échoué (outils de \
             développement Xcode à jour ? xcode-select --install)"
                .into(),
        ));
    }
    std::fs::rename(&staged, &binary)?;
    Ok(binary)
}

fn wait_with_timeout(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Result<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(PlatformError::Process(format!(
                "délai de {} s dépassé",
                timeout.as_secs()
            )));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_is_split_into_pages() {
        let raw = "\u{0C}page 1\nFacture 42\nTotal 12 €\n\u{0C}page 2\nConditions\n";
        let o = parse_output(raw);
        assert_eq!(o.pages, 2);
        assert_eq!(o.text, "Facture 42\nTotal 12 €\n\n\nConditions");
    }

    /// OCR réel d'un PDF sans couche texte : lent la première fois (compilation Swift),
    /// lancé à la main (`cargo test -p penelope-platform ocr -- --ignored`).
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn a_scanned_pdf_is_read_by_vision() {
        let dir = tempfile::tempdir().unwrap();
        let pdf = dir.path().join("scan.pdf");
        std::fs::write(&pdf, crate::ocr::tests::SCANNED_PDF).unwrap();
        let o = pdf_text(&pdf, dir.path(), 5, Duration::from_secs(300)).unwrap();
        assert_eq!(o.pages, 1);
        assert!(o.text.to_lowercase().contains("penelope"), "{}", o.text);
    }

    /// Page A6 dont le texte « PENELOPE OCR 2026 » n'existe qu'en image (pas de couche
    /// texte), générée à la main.
    #[cfg(target_os = "macos")]
    pub(crate) const SCANNED_PDF: &[u8] = include_bytes!("../tests/fixtures/scan.pdf");
}

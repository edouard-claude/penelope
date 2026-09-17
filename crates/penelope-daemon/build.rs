//! Documentation de la version compilée, embarquée dans le binaire (issue #34) : `README.md`
//! et chaque fichier Markdown de `docs/`, sans liste tenue à la main.

use std::path::{Path, PathBuf};

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|x| x == "md") {
            out.push(p);
        }
    }
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let root = manifest.join("../..");
    let docs = root.join("docs");
    println!("cargo:rerun-if-changed={}", docs.display());
    println!(
        "cargo:rerun-if-changed={}",
        root.join("README.md").display()
    );
    let mut files = vec![root.join("README.md")];
    collect(&docs, &mut files);
    files.retain(|f| f.is_file());
    files.sort();
    let mut code = String::from(
        "/// Fichiers de documentation embarqués : (chemin dans le dépôt, contenu).\npub const DOCS: &[(&str, &str)] = &[\n",
    );
    for f in &files {
        println!("cargo:rerun-if-changed={}", f.display());
        let rel = f
            .strip_prefix(&root)
            .unwrap_or(f)
            .to_string_lossy()
            .replace('\\', "/");
        let abs = std::fs::canonicalize(f).unwrap_or_else(|_| f.clone());
        code.push_str(&format!(
            "    ({rel:?}, include_str!({:?})),\n",
            abs.to_string_lossy()
        ));
    }
    code.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("docs.rs");
    std::fs::write(out, code).expect("écriture de docs.rs");
}

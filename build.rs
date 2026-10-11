//! Assemble the single-file web UI from `src/web/`.
//!
//! The server serves one HTML document; the sources are many files. The
//! template `src/web/index.html` has two markers, `<!-- @css -->` and
//! `<!-- @js -->`, each replaced by `src/web/css/*.css` or `src/web/js/*.js`
//! concatenated in file-name order (the numeric prefixes are the order).
//! `scripts/build_ui.py` is the same procedure for the dev server and the
//! fast UI deploy; keep the two in step.

use std::fs;
use std::path::Path;

fn concat(dir: &Path, ext: &str) -> String {
    let mut files: Vec<_> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().map_or(false, |x| x == ext))
        .collect();
    files.sort();
    let mut out = String::new();
    for f in files {
        println!("cargo:rerun-if-changed={}", f.display());
        out.push_str(&fs::read_to_string(&f).unwrap_or_else(|e| panic!("read {}: {e}", f.display())));
    }
    out
}

fn main() {
    let web = Path::new("src/web");
    println!("cargo:rerun-if-changed=src/web/index.html");
    println!("cargo:rerun-if-changed=src/web/css");
    println!("cargo:rerun-if-changed=src/web/js");
    let mut html = fs::read_to_string(web.join("index.html")).expect("read src/web/index.html");
    for (marker, sub, ext) in [("<!-- @css -->\n", "css", "css"), ("<!-- @js -->\n", "js", "js")] {
        assert!(html.contains(marker), "template lacks {marker}");
        html = html.replacen(marker, &concat(&web.join(sub), ext), 1);
    }
    let out = Path::new(&std::env::var("OUT_DIR").unwrap()).join("index.html");
    fs::write(&out, html).expect("write assembled index.html");
}

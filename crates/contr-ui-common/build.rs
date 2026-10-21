//! Compiles `data/` into the `GResource` bundle at the application's resource base path.

use std::fmt::Write as _;
use std::path::Path;
use std::{env, fs};

use contr_core::product;

fn main() {
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("data");
    let out = env::var("OUT_DIR").map(std::path::PathBuf::from).unwrap_or_default();
    let prefix = format!("/{}", product::APP_ID.replace('.', "/"));

    let mut files = String::new();
    let _ = writeln!(files, "    <file>style.css</file>");
    let _ = writeln!(
        files,
        "    <file alias=\"icons/scalable/apps/{}.svg\">icons/scalable/apps/app.svg</file>",
        product::APP_ID
    );
    let mut icons: Vec<_> = fs::read_dir(data.join("icons/scalable/actions"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    icons.sort();
    for icon in icons {
        let _ = writeln!(
            files,
            "    <file preprocess=\"xml-stripblanks\">icons/scalable/actions/{icon}</file>"
        );
    }

    let manifest = out.join("resources.gresource.xml");
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<gresources>\n  <gresource prefix=\"{prefix}\">\n{files}  </gresource>\n</gresources>\n"
    );
    if let Err(error) = fs::write(&manifest, xml) {
        println!("cargo::error=cannot write {}: {error}", manifest.display());
        return;
    }

    println!("cargo::rerun-if-changed=data");
    glib_build_tools::compile_resources(&[data], &manifest.to_string_lossy(), "resources.gresource");
}

use std::env;
use std::fs;
use std::io;
use std::path::Path;

fn embed(root: &Path, path: &Path, track: &str, output: &mut String) -> io::Result<()> {
    println!("cargo:rerun-if-changed={}", path.display());
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.is_dir() {
        let mut entries = fs::read_dir(path)?.collect::<io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            embed(root, &entry.path(), track, output)?;
        }
    } else if metadata.is_file() {
        let relative = path.strip_prefix(root).unwrap().to_str().unwrap();
        // Credentials are generated at runtime, never embedded from a developer's checkout.
        assert!(
            path.file_name().is_none_or(|name| name != ".env"),
            "Template must not contain .env"
        );
        output.push_str(&format!(
            "({track:?}, {relative:?}, include_bytes!({:?})),\n",
            path.to_str().unwrap()
        ));
    } else {
        panic!(
            "Template contains a symlink or special file: {}",
            path.display()
        );
    }
    Ok(())
}

fn main() -> io::Result<()> {
    let manifest = env::var_os("CARGO_MANIFEST_DIR").unwrap();
    let templates = Path::new(&manifest).join("../templates");
    println!("cargo:rerun-if-changed={}", templates.display());
    let mut output = String::from("const EMBEDDED: &[(&str, &str, &[u8])] = &[\n");
    for track in ["diligence", "sourcing", "portfolio", "funds"] {
        let root = templates.join(track);
        for part in ["template.json", "seed", "files"] {
            embed(&root, &root.join(part), track, &mut output)?;
        }
    }
    output.push_str("];");
    fs::write(
        Path::new(&env::var_os("OUT_DIR").unwrap()).join("templates.rs"),
        output,
    )
}

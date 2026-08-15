use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());

    let agents_md = copy_doc(
        &manifest_dir,
        &out_dir,
        &[
            PathBuf::from("../../docs/agents.md"),
            PathBuf::from("docs/agents.md"),
        ],
        "agents.md",
    );
    let schema_v1 = copy_doc(
        &manifest_dir,
        &out_dir,
        &[
            PathBuf::from("../../docs/schema/v1.json"),
            PathBuf::from("docs/schema/v1.json"),
        ],
        "schema-v1.json",
    );

    println!(
        "cargo:rustc-env=TICGIT_AGENTS_MD_PATH={}",
        agents_md.display()
    );
    println!(
        "cargo:rustc-env=TICGIT_SCHEMA_V1_PATH={}",
        schema_v1.display()
    );

    // Vendor assets for the `ti serve` flow view (React + @xyflow/react
    // UMD bundles, served locally so there are no CDN dependencies).
    let flow_vendor = manifest_dir.join("vendor/flow");
    for &name in &[
        "react.min.js",
        "react-dom.min.js",
        "xyflow.min.js",
        "xyflow.css",
        "jsx-runtime-shim.js",
    ] {
        let src = flow_vendor.join(name);
        println!("cargo:rerun-if-changed={}", src.display());
        let dst = out_dir.join(name);
        fs::copy(&src, &dst).unwrap_or_else(|error| {
            panic!(
                "failed to copy vendor asset {} to {}: {error}",
                src.display(),
                dst.display()
            )
        });
        println!("cargo:rustc-env=TICGIT_VENDOR_{}={}", name.replace(['.', '-'], "_").to_uppercase(), dst.display());
    }
}

fn copy_doc(
    manifest_dir: &Path,
    out_dir: &Path,
    candidates: &[PathBuf],
    output_name: &str,
) -> PathBuf {
    let existing_sources: Vec<_> = candidates
        .iter()
        .map(|relative_path| manifest_dir.join(relative_path))
        .filter(|source| source.exists())
        .collect();

    if existing_sources.len() > 1 {
        let baseline = fs::read(&existing_sources[0]).unwrap_or_else(|error| {
            panic!("failed to read {}: {error}", existing_sources[0].display())
        });
        for source in existing_sources.iter().skip(1) {
            let contents = fs::read(source)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", source.display()));
            if contents != baseline {
                panic!(
                    "documentation asset {} differs from {}",
                    source.display(),
                    existing_sources[0].display()
                );
            }
        }
    }

    for relative_path in candidates {
        let source = manifest_dir.join(relative_path);
        println!("cargo:rerun-if-changed={}", source.display());
        if source.exists() {
            let destination = out_dir.join(output_name);
            fs::copy(&source, &destination).unwrap_or_else(|error| {
                panic!(
                    "failed to copy {} to {}: {error}",
                    source.display(),
                    destination.display()
                )
            });
            return destination;
        }
    }

    panic!("could not find packaged documentation asset {output_name}");
}

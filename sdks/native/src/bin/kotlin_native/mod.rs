//! Kotlin/Native templates over the pinned UniFFI metadata and Swift C header.
mod jna;
mod render;

use anyhow::{Context, Result, ensure};
use camino::Utf8Path;
use uniffi_bindgen::{BindgenLoader, BindgenPaths, GlobalConfig};

pub fn generate(library: &Utf8Path, out: &Utf8Path) -> Result<()> {
    let loader = BindgenLoader::new(BindgenPaths::default(), GlobalConfig::default());
    let metadata = loader.load_metadata(library)?;
    let mut cis = loader.load_cis(metadata)?;
    ensure!(cis.len() == 1, "expected one ABI component");
    let ci = cis.first_mut().context("missing ABI component")?;
    ci.derive_ffi_funcs()?;
    ensure!(
        ci.crate_name() == "rocky_uniffi",
        "expected the Rocky ABI component"
    );
    for type_ in ci.iter_local_types() {
        ensure!(
            !matches!(
                type_,
                uniffi_bindgen::interface::Type::Set { .. }
                    | uniffi_bindgen::interface::Type::Box { .. }
                    | uniffi_bindgen::interface::Type::Custom { .. }
            ),
            "unsupported native binding type: {type_:?}"
        );
    }
    for object in ci.object_definitions() {
        ensure!(
            object.trait_impls().is_empty(),
            "native trait implementations are unsupported"
        );
    }
    std::fs::create_dir_all(out)?;
    uniffi_bindgen::bindings::generate(uniffi_bindgen::bindings::GenerateOptions {
        source: library.to_owned(),
        out_dir: out.to_owned(),
        format: false,
        languages: vec![uniffi_bindgen::bindings::TargetLanguage::Swift],
        metadata_no_deps: true,
        ..Default::default()
    })?;
    std::fs::remove_file(out.join("rocky_uniffi.swift"))?;
    std::fs::remove_file(out.join("rocky_uniffiFFI.modulemap"))?;
    let config = toml::from_str::<render::Config>(
        r#"package_name = "rocky.internal.native"
cdylib_name = "rocky_uniffi"
kotlin_multiplatform = true
kotlin_targets = ["native"]
generate_immutable_records = true
use_pascal_case_enum_class = true
kotlin_target_version = "2.4.20"
"#,
    )?;
    let bindings = render::generate_bindings(&config, ci)?;
    std::fs::create_dir_all(out)?;
    std::fs::write(out.join("RockyTypes.kt"), bindings.common)?;
    std::fs::write(
        out.join("RockyNative.kt"),
        inherit_throws(&bindings.native.context("missing native binding")?),
    )?;
    let jna_dir = out.join("jna");
    uniffi_bindgen::bindings::generate(uniffi_bindgen::bindings::GenerateOptions {
        source: library.to_owned(),
        out_dir: jna_dir.clone(),
        format: false,
        languages: vec![uniffi_bindgen::bindings::TargetLanguage::Kotlin],
        metadata_no_deps: true,
        config_override: Some("sdks/native/uniffi.toml".into()),
        ..Default::default()
    })?;
    jna::adapt(&jna_dir.join("rocky/internal/native/rocky_uniffi.kt"), ci)?;
    Ok(())
}

// An override inherits its interface's `@Throws`; repeating it fails the shared iOS metadata
// compilation, which resolves the common filter's classes differently.
fn inherit_throws(source: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    for line in source.lines() {
        if line.contains("actual override ")
            && lines
                .last()
                .is_some_and(|previous| previous.trim_start().starts_with("@Throws("))
        {
            lines.pop();
        }
        lines.push(line);
    }
    lines.join("\n") + "\n"
}

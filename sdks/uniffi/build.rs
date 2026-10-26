use quote::quote;
use std::{env, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut bindings = proc_macro2::TokenStream::new();
    let mut names = Vec::new();
    // Status declarations, and the blocker types status reuses from rocky-core.
    let sources = [
        "../../crates/rocky-sdk/src/status.rs",
        "../../crates/rocky-core/src/blocker.rs",
    ];
    let mut items = Vec::new();
    for source in sources {
        println!("cargo:rerun-if-changed={source}");
        items.extend(syn::parse_file(&fs::read_to_string(source)?)?.items);
    }
    for item in items {
        match item {
            syn::Item::Struct(mut record) if matches!(record.vis, syn::Visibility::Public(_)) => {
                names.push(record.ident.to_string());
                record.attrs.retain(|attr| attr.path().is_ident("doc"));
                for field in &mut record.fields {
                    field.attrs.retain(|attr| attr.path().is_ident("doc"));
                }
                bindings.extend(quote! { #[uniffi::remote(Record)] #record });
            }
            syn::Item::Enum(mut variants) if matches!(variants.vis, syn::Visibility::Public(_)) => {
                names.push(variants.ident.to_string());
                variants.attrs.retain(|attr| attr.path().is_ident("doc"));
                bindings.extend(quote! { #[uniffi::remote(Enum)] #variants });
            }
            _ => {}
        }
    }
    fs::write(
        PathBuf::from(env::var("OUT_DIR")?).join("status.rs"),
        bindings.to_string(),
    )?;
    fs::write(
        PathBuf::from(env::var("OUT_DIR")?).join("status_names.rs"),
        quote! { const STATUS_TYPE_NAMES: &[&str] = &[#(#names),*]; }.to_string(),
    )?;
    Ok(())
}

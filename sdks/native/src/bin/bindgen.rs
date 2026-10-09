mod kotlin_native;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|arg| arg == "generate-native") {
        if args.len() != 4 {
            eprintln!("usage: rocky-bindgen generate-native LIBRARY OUT_DIR");
            std::process::exit(2);
        }
        if let Err(error) =
            kotlin_native::generate(args[2].as_str().into(), args[3].as_str().into())
        {
            eprintln!("Kotlin/Native binding: {error:#}");
            std::process::exit(1);
        }
    } else {
        uniffi::uniffi_bindgen_main();
    }
}

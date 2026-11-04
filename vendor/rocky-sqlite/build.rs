// Rocky: the SQLite amalgamation in `sqlite3/` is libsqlite3-sys 0.38.2's,
// the one native Rocky bundles, and the flags are its bundled flags. Only
// what cannot exist in a browser differs: no OS layer (`SQLITE_OS_OTHER`;
// the VFS is registered at runtime), no threads, no extension loading, no
// temporary files, no `localtime_r` or `usleep`.
const FLAGS: &[&str] = &[
    "-DSQLITE_CORE",
    "-DSQLITE_DEFAULT_FOREIGN_KEYS=1",
    "-DSQLITE_ENABLE_API_ARMOR",
    "-DSQLITE_ENABLE_COLUMN_METADATA",
    "-DSQLITE_ENABLE_DBSTAT_VTAB",
    "-DSQLITE_ENABLE_FTS3",
    "-DSQLITE_ENABLE_FTS3_PARENTHESIS",
    "-DSQLITE_ENABLE_FTS5",
    "-DSQLITE_ENABLE_JSON1",
    "-DSQLITE_ENABLE_MEMORY_MANAGEMENT",
    "-DSQLITE_ENABLE_PREUPDATE_HOOK",
    "-DSQLITE_ENABLE_RTREE",
    "-DSQLITE_ENABLE_STAT4",
    "-DSQLITE_SOUNDEX",
    "-DSQLITE_USE_URI",
    "-DHAVE_ISNAN",
    // Browser-only.
    "-DSQLITE_OS_OTHER",
    "-DSQLITE_THREADSAFE=0",
    "-DSQLITE_OMIT_LOAD_EXTENSION",
    "-DSQLITE_TEMP_STORE=3",
];

/// Natively libsqlite3-sys compiles SQLite. In a browser this archive is
/// named `libsqlite3.a`: libsqlite3-sys, built without `bundled`, links
/// `-lsqlite3`, and the search path emitted here resolves it, so no consumer
/// configuration is needed.
fn main() {
    println!("cargo::rerun-if-changed=shim");
    println!("cargo::rerun-if-changed=sqlite3");
    let var = |name| std::env::var(name).unwrap_or_default();
    if var("CARGO_CFG_TARGET_FAMILY") == "wasm" && var("CARGO_CFG_TARGET_OS") == "unknown" {
        compile();
    }
}

/// The toolchain's `llvm-ar` (rustup component `llvm-tools`). A host `ar`
/// such as Apple's writes no symbol index for wasm objects, and rust-lld then
/// finds no SQLite symbols. `AR_wasm32_unknown_unknown` still overrides it.
// The host runs build scripts; wasm32's clippy configuration does not apply.
#[allow(clippy::disallowed_methods)]
fn toolchain_archiver() -> Option<std::path::PathBuf> {
    if std::env::var_os("AR_wasm32_unknown_unknown").is_some() {
        return None;
    }
    let rustc = std::env::var_os("RUSTC")?;
    let output = std::process::Command::new(rustc)
        .args(["--print", "sysroot"])
        .output()
        .ok()?;
    let sysroot = String::from_utf8(output.stdout).ok()?;
    let host = std::env::var("HOST").ok()?;
    let archiver = std::path::Path::new(sysroot.trim())
        .join("lib/rustlib")
        .join(host)
        .join("bin/llvm-ar");
    archiver.exists().then_some(archiver)
}

fn compile() {
    const C_SOURCE: [&str; 36] = [
        // string
        "string/memchr.c",
        "string/memrchr.c",
        "string/stpcpy.c",
        "string/stpncpy.c",
        "string/strcat.c",
        "string/strchr.c",
        "string/strchrnul.c",
        "string/strcmp.c",
        "string/strcpy.c",
        "string/strcspn.c",
        "string/strlen.c",
        "string/strncat.c",
        "string/strncmp.c",
        "string/strncpy.c",
        "string/strrchr.c",
        "string/strspn.c",
        // stdlib
        "stdlib/atoi.c",
        "stdlib/bsearch.c",
        "stdlib/qsort.c",
        "stdlib/qsort_nr.c",
        "stdlib/strtod.c",
        "stdlib/strtol.c",
        // math
        "math/__fpclassifyl.c",
        "math/acosh.c",
        "math/asinh.c",
        "math/atanh.c",
        "math/fmodl.c",
        "math/scalbn.c",
        "math/scalbnl.c",
        "math/sqrt.c",
        "math/trunc.c",
        // errno
        "errno/__errno_location.c",
        // stdio
        "stdio/__toread.c",
        "stdio/__uflow.c",
        // internal
        "internal/floatscan.c",
        "internal/shgetc.c",
    ];

    const SQLITE3_SOURCE: &str = "sqlite3/sqlite3.c";

    let mut cc = cc::Build::new();
    cc.warnings(false)
        .flag("-Wno-macro-redefined")
        .include("shim")
        .include("shim/musl/arch/generic")
        .include("shim/musl/include")
        .file("shim/printf/printf.c")
        .file(SQLITE3_SOURCE)
        .files(C_SOURCE.map(|s| format!("shim/musl/{s}")))
        .flag("-DPRINTF_ALIAS_STANDARD_FUNCTION_NAMES_HARD")
        .flag("-include")
        .flag("shim/wasm-shim.h");

    for flag in FLAGS {
        cc.flag(flag);
    }
    if let Some(archiver) = toolchain_archiver() {
        cc.archiver(archiver);
    }

    cc.compile("sqlite3");
}

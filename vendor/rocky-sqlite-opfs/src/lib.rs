mod locks;

#[expect(
    non_snake_case,
    reason = "VFS callbacks retain SQLite's C ABI names and parameter names."
)]
pub mod sahpool;

#[cfg(all(test, target_arch = "wasm32"))]
wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_dedicated_worker);

//! WASM bridge conventions (FEAT-016, ADR-0019): the one place that knows how a
//! `#[wasm_bindgen]` export is named on the JavaScript side, how a wasm package is keyed, and
//! how packages are discovered from `Cargo.toml` and `package.json`. It depends on the model
//! only; `katalogr-ext-rust` and `katalogr-ext-ts` call it, so both compute the same contract.

pub mod attr;
pub mod packages;
pub mod pkg;
pub mod rust;

/// The contract kind every wasm-bindgen export binds to.
pub const CONTRACT_KIND: &str = "wasm_export";
/// Symbol fact: the JS path a Rust item is exported as (`greet`, `Counter.increment`), or `(start)`.
pub const FACT_JS: &str = "wasm_js";
/// Symbol fact: what a `#[wasm_bindgen] extern` item imports, `<module>:<js path>` or `global:<js path>`.
pub const FACT_IMPORT: &str = "wasm_import";
/// wasm-pack's default output directory, relative to the crate directory.
pub const DEFAULT_OUT_DIR: &str = "pkg";
pub const CARGO_TOML: &str = "Cargo.toml";
pub const PACKAGE_JSON: &str = "package.json";

pub use packages::{Output, WasmPackage, WasmPackages};

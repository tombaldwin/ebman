//! Bakes the current version's release date into the binary.
//!
//! Parsed from `CHANGELOG.md` rather than kept in a constant, so there
//! is no second place to remember to bump. `CHANGELOG.md` ships inside
//! the published crate (`Cargo.toml`'s `exclude` drops only
//! `mutants.out*` and `target`), so this works for `cargo install` as
//! well as for a git checkout.

include!("src/release_meta.rs");

fn main() {
    println!("cargo:rerun-if-changed=CHANGELOG.md");
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let date = std::fs::read_to_string("CHANGELOG.md")
        .ok()
        .and_then(|body| release_date_for(&body, &version).map(str::to_string))
        .unwrap_or_default();
    // Empty means "unknown" — the UI shows the version without a date
    // rather than inventing one.
    println!("cargo:rustc-env=EBMAN_RELEASE_DATE={date}");
}

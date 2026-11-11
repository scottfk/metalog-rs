//! Link the same C libraries the upstream build links: libpcre2-8 always,
//! zlib with the `compress` feature.  They're declared by hand in
//! `src/sys/` rather than through -sys crates, to keep `libc` the only
//! dependency (the toolchain is offline-friendly Gentoo stable).

fn main() {
    println!("cargo:rustc-link-lib=pcre2-8");
    if std::env::var_os("CARGO_FEATURE_COMPRESS").is_some() {
        println!("cargo:rustc-link-lib=z");
    }
    println!("cargo:rerun-if-changed=build.rs");
}

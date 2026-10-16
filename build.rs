//! The C layer over llama.cpp (csrc/embed.c), against the llama.cpp built in
//! vendor/ (scripts/build-llama.sh). The program finds its libraries beside it,
//! in ../lib/sonda once installed, or in that build while it is worked on.

fn main() {
    let root = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let llama = format!("{root}/vendor/llama.cpp");
    let libs = std::env::var("SONDA_LLAMA_LIBS").unwrap_or_else(|_| format!("{root}/vendor/llama-build/bin"));
    cc::Build::new()
        .file("csrc/embed.c")
        .include(format!("{llama}/include"))
        .include(format!("{llama}/ggml/include"))
        .include(format!("{llama}/tools/mtmd"))
        .flag_if_supported("-std=c11")
        .warnings(true)
        .compile("sonda_embed");
    println!("cargo:rerun-if-changed=csrc/embed.c");
    println!("cargo:rerun-if-env-changed=SONDA_LLAMA_LIBS");
    println!("cargo:rustc-link-search=native={libs}");
    for lib in ["mtmd", "llama", "ggml", "ggml-base"] {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }
    println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/../lib/sonda");
    println!("cargo:rustc-link-arg=-Wl,-rpath,{libs}");
}

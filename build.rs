//! Surfaces the build-time `TARGET` triple to the binary so
//! `ampelos update` can request the right release asset at runtime.

fn main() {
    let target = std::env::var("TARGET").expect("TARGET set by cargo");
    println!("cargo:rustc-env=AMPELOS_TARGET={target}");
}

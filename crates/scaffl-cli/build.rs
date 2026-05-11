//! Surfaces the build-time `TARGET` triple to the binary so
//! `scaffl update` can request the right release asset at runtime.

fn main() {
    let target = std::env::var("TARGET").expect("TARGET set by cargo");
    println!("cargo:rustc-env=SCAFFL_TARGET={target}");
}

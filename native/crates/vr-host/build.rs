// Names the target this binary is built for (the update asks the hub for the same one) and, when native/build.sh
// passes them, the VibeRoller release it belongs to.
fn main() {
    println!("cargo:rustc-env=VR_TARGET={}", std::env::var("TARGET").unwrap_or_default());
    for k in ["VR_VERSION", "VR_COMMIT", "VR_RELEASE_PUBKEY"] { println!("cargo:rerun-if-env-changed={k}"); }
}

// The host runner names the target it is built for (its update asks the hub for the same one) and, when
// native/build.sh passes them, the release it belongs to: the same as vr-host's build.rs.
fn main() {
    println!("cargo:rustc-env=VR_TARGET={}", std::env::var("TARGET").unwrap_or_default());
    for k in ["VR_VERSION", "VR_COMMIT", "VR_RELEASE_PUBKEY"] { println!("cargo:rerun-if-env-changed={k}"); }
}

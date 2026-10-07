// Re-run the build (re-embedding `sqlx::migrate!`) when migrations change.
fn main() {
    println!("cargo:rerun-if-changed=../../migrations");
}

// Migrations are embedded at compile time by `sqlx::migrate!`; make sure a
// new or edited migration file triggers a rebuild.
fn main() {
    println!("cargo:rerun-if-changed=src/database/migrations");
}

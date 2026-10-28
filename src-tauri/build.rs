fn main() {
    println!("cargo:rerun-if-changed=../ui/index.html");
    tauri_build::build()
}

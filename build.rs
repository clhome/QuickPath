fn main() {
    println!("cargo:rerun-if-changed=quickpath.rc");
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rerun-if-changed=assets/logo.ico");
    let _ = embed_resource::compile("quickpath.rc", embed_resource::NONE);

}

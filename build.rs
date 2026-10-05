fn main() {
    println!("cargo:rerun-if-changed=quickpath.rc");
    println!("cargo:rerun-if-changed=app.manifest");
    let _ = embed_resource::compile("quickpath.rc", embed_resource::NONE);
}

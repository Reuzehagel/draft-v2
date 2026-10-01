// Embeds the application icon (`assets/draft.ico`, drawn by `src/mark.rs`)
// in both executables, so Explorer, the Start menu and the taskbar show the
// mark instead of Windows' generic program icon.

fn main() {
    println!("cargo:rerun-if-changed=assets/draft.rc");
    println!("cargo:rerun-if-changed=assets/draft.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("assets/draft.rc", embed_resource::NONE)
            .manifest_required()
            .expect("compile the icon resource");
    }
}

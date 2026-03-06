use std::env;

fn main() {
    emit_user_linker_script("ring3_doom");
}

fn emit_user_linker_script(bin_name: &str) {
    let target = env::var("TARGET").unwrap_or_else(|_| String::new());
    let script = match target.as_str() {
        "x86_64-unknown-none" => "user/user_x86_64.ld",
        "aarch64-unknown-none" => "user/user_aarch64.ld",
        _ => return,
    };
    println!("cargo:rustc-link-arg-bin={bin_name}=-T{script}");
    println!("cargo:rerun-if-changed={script}");
}

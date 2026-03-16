// user/doom/build.rs: linker script + optional C DoomGeneric compilation for
// M32 fully-userland Doom.
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rustc-check-cfg=cfg(arrost_userland_doom)");
    emit_user_linker_script("ring3_doom");

    let manifest_dir = PathBuf::from(
        env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set by cargo"),
    );
    // repo root is two levels up from user/doom/
    let repo_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("user/doom crate must live inside workspace root");

    let doomgeneric_ready = env::var("ARROST_DOOM_GENERIC_READY")
        .map(|v| v == "true")
        .unwrap_or(false);

    let include_dir = repo_root.join("user/doom/third_party/doomgeneric/doomgeneric");
    let header_present = include_dir.join("doomgeneric.h").exists();
    let keys_present = include_dir.join("doomkeys.h").exists();
    let core_source = include_dir.join("doomgeneric.c");
    let core_present = core_source.exists();
    let makefile_soso = include_dir.join("Makefile.soso");
    let makefile_present = makefile_soso.exists();

    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_else(|_| "x86_64".to_string());
    let clang_target = match target_arch.as_str() {
        "x86_64" => "x86_64-unknown-none-elf",
        "aarch64" => "aarch64-unknown-none",
        _ => "x86_64-unknown-none-elf",
    };
    let archiver_wrapper = repo_root.join("scripts/ar-wrapper.sh");

    let c_dir = repo_root.join("user/doom/c");
    let shim_include = c_dir.join("freestanding_include");
    let libc_shim = c_dir.join("freestanding_libc.c");
    let runner = c_dir.join("doomgeneric_runner.c");
    let platform_userland = c_dir.join("doomgeneric_arrost_userland.c");
    let audio_stub = c_dir.join("doomgeneric_audio_stub.c");
    let audio_userland = c_dir.join("doomgeneric_audio_userland.c");
    let callbacks_userland = c_dir.join("doomgeneric_callbacks_userland.c");
    let opl2 = c_dir.join("opl/opl2.c");

    let use_real_bridge =
        doomgeneric_ready && header_present && keys_present && core_present && makefile_present;

    if use_real_bridge {
        let mut core_files = parse_makefile_sources(&makefile_soso, &include_dir);
        core_files.retain(|path| {
            !path.ends_with("doomgeneric_soso.c")
                && !path.ends_with("doomgeneric_sosox.c")
                && !path.ends_with("doomgeneric_xlib.c")
                && !path.ends_with("doomgeneric_sdl.c")
                && !path.ends_with("doomgeneric_linuxvt.c")
                && !path.ends_with("doomgeneric_win.c")
                && !path.ends_with("doomgeneric_allegro.c")
                && !path.ends_with("doomgeneric_emscripten.c")
        });

        let mut build = cc::Build::new();
        build
            .compiler("clang")
            .archiver(&archiver_wrapper)
            .flag(format!("--target={clang_target}"))
            .flag("-std=c11")
            .flag("-ffreestanding")
            .flag("-fno-builtin")
            .flag("-fno-stack-protector")
            .flag("-fPIC")
            .flag("-ffunction-sections")
            .flag("-fdata-sections")
            .warnings(false)
            .define("NORMALUNIX", None)
            .define("LINUX", None)
            .define("D_DEFAULT_SOURCE", None)
            .define("FEATURE_SOUND", None)
            .define("ARROST_USERLAND", None)
            .define("DOOMGENERIC_RESX", "320")
            .define("DOOMGENERIC_RESY", "200")
            .include(&shim_include)
            .include(&include_dir)
            .include(&c_dir);

        if target_arch == "x86_64" {
            build.flag("-mno-red-zone");
        }

        // Freestanding libc shim
        build.file(&libc_shim);
        // DoomGeneric runner (create/tick wrapper)
        build.file(&runner);
        // Userland platform glue (DG_DrawFrame etc via syscalls)
        build.file(&platform_userland);
        // Audio stub (mixer + OPL2 music)
        build.file(&audio_stub);
        // Userland audio callbacks (arr_dg_audio_pcm16 etc via syscalls)
        build.file(&audio_userland);
        // Userland callback implementations (WAD loading, logging, cfg via syscalls)
        build.file(&callbacks_userland);
        // OPL2 FM synthesiser
        build.file(&opl2);
        // DoomGeneric core sources from Makefile.soso
        for file in &core_files {
            build.file(file);
            println!("cargo:rerun-if-changed={}", file.display());
        }

        println!("cargo:rustc-cfg=arrost_userland_doom");
        build.compile("arrost_userland_doom");

        // Force the linker to pull all symbols from the C archive.
        // Without --whole-archive, --as-needed may skip the library
        // when it appears before the Rust objects that reference it.
        let out_dir = env::var("OUT_DIR").unwrap();
        println!("cargo:rustc-link-arg-bin=ring3_doom=--whole-archive");
        println!("cargo:rustc-link-arg-bin=ring3_doom=-L{out_dir}");
        println!("cargo:rustc-link-arg-bin=ring3_doom=-larrost_userland_doom");
        println!("cargo:rustc-link-arg-bin=ring3_doom=--no-whole-archive");
    }

    // rerun-if-changed / rerun-if-env-changed
    println!("cargo:rerun-if-env-changed=ARROST_DOOM_GENERIC_READY");
    println!("cargo:rerun-if-changed={}", libc_shim.display());
    println!("cargo:rerun-if-changed={}", runner.display());
    println!("cargo:rerun-if-changed={}", platform_userland.display());
    println!("cargo:rerun-if-changed={}", audio_stub.display());
    println!("cargo:rerun-if-changed={}", audio_userland.display());
    println!("cargo:rerun-if-changed={}", callbacks_userland.display());
    println!("cargo:rerun-if-changed={}", opl2.display());
    println!(
        "cargo:rerun-if-changed={}",
        c_dir.join("opl/opl2.h").display()
    );
    println!("cargo:rerun-if-changed={}", core_source.display());
    println!("cargo:rerun-if-changed={}", makefile_soso.display());
    println!("cargo:rerun-if-changed={}", shim_include.display());
    println!(
        "cargo:rerun-if-changed={}",
        shim_include.join("stdlib.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        shim_include.join("stdio.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        include_dir.join("doomgeneric.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        include_dir.join("doomkeys.h").display()
    );
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

fn parse_makefile_sources(makefile: &Path, source_root: &Path) -> Vec<PathBuf> {
    let content = match fs::read_to_string(makefile) {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };
    let Some(raw_list) = content
        .lines()
        .find(|line| line.trim_start().starts_with("SRC_DOOM ="))
        .map(|line| {
            line.split_once('=')
                .map(|(_, right)| right.trim())
                .unwrap_or("")
        })
    else {
        return Vec::new();
    };

    raw_list
        .split_whitespace()
        .filter_map(|token| token.strip_suffix(".o"))
        .map(|stem| source_root.join(format!("{stem}.c")))
        .collect()
}

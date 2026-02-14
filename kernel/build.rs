// kernel/build.rs: compile the C DoomGeneric bridge for kernel-side M10.x runtime wiring.
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rustc-check-cfg=cfg(arrost_doomgeneric_bridge)");

    let manifest_dir = PathBuf::from(
        env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set by cargo"),
    );
    let repo_root = manifest_dir
        .parent()
        .expect("kernel crate must live inside workspace root");
    emit_wad_embed(repo_root);

    let doomgeneric_ready = env::var("ARROST_DOOM_GENERIC_READY")
        .map(|value| value == "true")
        .unwrap_or(false);
    let include_dir = repo_root.join("user/doom/third_party/doomgeneric/doomgeneric");
    let core_source = include_dir.join("doomgeneric.c");
    let makefile_soso = include_dir.join("Makefile.soso");
    let header_present = include_dir.join("doomgeneric.h").exists();
    let keys_present = include_dir.join("doomkeys.h").exists();
    let core_present = core_source.exists();
    let makefile_present = makefile_soso.exists();

    let mut build = cc::Build::new();
    build
        .compiler("clang")
        .flag("--target=x86_64-unknown-none-elf")
        .flag("-std=c11")
        .flag("-ffreestanding")
        .flag("-fno-builtin")
        .flag("-fno-stack-protector")
        .flag("-mno-red-zone")
        .flag("-ffunction-sections")
        .flag("-fdata-sections")
        .flag("-Wall")
        .flag("-Wextra")
        .define("NORMALUNIX", None)
        .define("LINUX", None)
        .define("D_DEFAULT_SOURCE", None)
        .define("FEATURE_SOUND", None);

    let c_dir = repo_root.join("user/doom/c");
    let runner = c_dir.join("doomgeneric_runner.c");
    let platform = c_dir.join("doomgeneric_arrost.c");
    let audio_stub = c_dir.join("doomgeneric_audio_stub.c");
    let libc_shim = c_dir.join("freestanding_libc.c");
    let stub = c_dir.join("doomgeneric_runner_stub.c");
    let shim_include = c_dir.join("freestanding_include");

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

        build
            .include(&shim_include)
            .include(&include_dir)
            // DoomGeneric i_video path expects a framebuffer at least 320x200.
            // Using smaller values leads to fb_scaling=0 and black frames.
            .define("DOOMGENERIC_RESX", "320")
            .define("DOOMGENERIC_RESY", "200")
            .file(&libc_shim)
            .file(&runner)
            .file(&audio_stub)
            .file(&platform);
        for file in &core_files {
            build.file(file);
            println!("cargo:rerun-if-changed={}", file.display());
        }
        println!("cargo:rustc-cfg=arrost_doomgeneric_bridge");
    } else {
        build.file(&stub);
        println!("cargo:warning=building doomgeneric bridge stub (missing ready headers/env)");
    }

    build.compile("arrost_doomgeneric_bridge");

    println!("cargo:rerun-if-env-changed=ARROST_DOOM_GENERIC_READY");
    println!("cargo:rerun-if-env-changed=ARROST_DOOM_WAD_HINT");
    println!("cargo:rerun-if-env-changed=ARROST_DOOM_WAD_PRESENT");
    println!("cargo:rerun-if-changed={}", runner.display());
    println!("cargo:rerun-if-changed={}", platform.display());
    println!("cargo:rerun-if-changed={}", audio_stub.display());
    println!("cargo:rerun-if-changed={}", libc_shim.display());
    println!("cargo:rerun-if-changed={}", stub.display());
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
        shim_include.join("strings.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        shim_include.join("stdint.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        shim_include.join("inttypes.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        shim_include.join("SDL_mixer.h").display()
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

fn emit_wad_embed(repo_root: &Path) {
    let out_dir =
        PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR must be set by cargo for build scripts"));
    let generated = out_dir.join("doom_wad_embed.rs");

    let wad_hint = env::var("ARROST_DOOM_WAD_HINT")
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                repo_root.join(path)
            }
        })
        .unwrap_or_else(|_| repo_root.join("user/doom/wad/doom1.wad"));
    let wad_present = env::var("ARROST_DOOM_WAD_PRESENT")
        .map(|value| value == "true")
        .unwrap_or(false)
        && wad_hint.exists();

    if wad_present {
        let escaped = wad_hint
            .display()
            .to_string()
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        let content = format!(
            "// Generated by kernel/build.rs\npub static ARROST_DOOM_WAD_BYTES: &[u8] = include_bytes!(\"{}\");\n",
            escaped
        );
        let _ = fs::write(&generated, content);
        println!("cargo:rerun-if-changed={}", wad_hint.display());
    } else {
        let _ = fs::write(
            &generated,
            "// Generated by kernel/build.rs\npub static ARROST_DOOM_WAD_BYTES: &[u8] = &[];\n",
        );
    }
}

use anyhow::{Context, Result, bail};
use bootloader::DiskImageBuilder;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const KERNEL_TARGET: &str = "x86_64-unknown-none";
const KERNEL_PACKAGE: &str = "arrost-kernel";
const USER_INIT_PACKAGE: &str = "arrost-user-init";
const USER_DOOM_PACKAGE: &str = "arrost-user-doom";
const BUILD_STD: &str = "-Zbuild-std=core,compiler_builtins,alloc";
const BUILD_STD_FEATURES: &str = "-Zbuild-std-features=compiler-builtins-mem";
const M6_DISK_SIZE_BYTES: u64 = 16 * 1024 * 1024;
const VERSION_MAJOR: u64 = 0;
const VERSION_MINOR: u64 = 1;
const BUILD_COUNTER_FILE: &str = ".arrost_build_count";
const DOOM_C_SOURCE: &str = "user/doom/c/doom_backend.c";
const DOOM_GENERIC_ROOT: &str = "user/doom/third_party/doomgeneric";
const DOOM_GENERIC_CORE_SOURCE: &str =
    "user/doom/third_party/doomgeneric/doomgeneric/doomgeneric.c";
const DOOM_GENERIC_INCLUDE_DIR: &str = "user/doom/third_party/doomgeneric/doomgeneric";
const DOOM_GENERIC_PORT_SOURCE: &str = "user/doom/c/doomgeneric_arrost.c";
const DOOM_WAD_HINT: &str = "user/doom/wad/doom1.wad";
const DOOM_FORCE_FALLBACK_ENV: &str = "ARROST_DOOM_FORCE_FALLBACK";

struct UserArtifact {
    hint: PathBuf,
    size: u64,
}

struct DoomCBackendArtifact {
    object: PathBuf,
    size: u64,
    ready: bool,
}

struct DoomGenericArtifact {
    root: PathBuf,
    core_source: PathBuf,
    core_object: PathBuf,
    core_size: u64,
    core_ready: bool,
    port_object: PathBuf,
    port_size: u64,
    port_ready: bool,
    ready: bool,
    wad_hint: PathBuf,
    wad_present: bool,
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("build") => build(),
        Some("run") => run_qemu(),
        Some("smoke-doom") => smoke_doom(),
        Some("smoke-doom-long") => smoke_doom_long(),
        Some("smoke-doom-virtio") => smoke_doom_virtio(),
        Some("smoke-doom-fallback") => smoke_doom_fallback(),
        _ => {
            eprintln!(
                "Usage: cargo xtask <build|run|smoke-doom|smoke-doom-long|smoke-doom-virtio|smoke-doom-fallback>"
            );
            Ok(())
        }
    }
}

fn build() -> Result<()> {
    build_impl(env_truthy(DOOM_FORCE_FALLBACK_ENV))
}

fn build_impl(force_fallback: bool) -> Result<()> {
    let build_count = next_build_count()?;
    let version = format!("{VERSION_MAJOR}.{VERSION_MINOR}.{build_count}");
    let build_count_env = build_count.to_string();
    let major_env = VERSION_MAJOR.to_string();
    let minor_env = VERSION_MINOR.to_string();
    println!("ArrOSt build version: {version}");

    let user_init =
        build_userland_package(USER_INIT_PACKAGE, &build_count_env, &major_env, &minor_env)?;
    let user_doom =
        build_userland_package(USER_DOOM_PACKAGE, &build_count_env, &major_env, &minor_env)?;
    let doom_c_backend = build_doom_c_backend_artifact()?;
    let doom_generic = build_doom_generic_artifact()?;
    println!(
        "ArrOSt doom backend object: ready={} path={} size={}",
        doom_c_backend.ready,
        doom_c_backend.object.display(),
        doom_c_backend.size
    );
    println!(
        "ArrOSt doomgeneric: ready={} root={} core={} core_obj={} core_size={} core_ready={} port={} port_size={} port_ready={} wad={} wad_present={}",
        doom_generic.ready,
        doom_generic.root.display(),
        doom_generic.core_source.display(),
        doom_generic.core_object.display(),
        doom_generic.core_size,
        doom_generic.core_ready,
        doom_generic.port_object.display(),
        doom_generic.port_size,
        doom_generic.port_ready,
        doom_generic.wad_hint.display(),
        doom_generic.wad_present
    );
    let doom_generic_ready_for_kernel = doom_generic.ready && !force_fallback;
    if force_fallback {
        println!(
            "ArrOSt doomgeneric: forcing ready=false for kernel metadata ({DOOM_FORCE_FALLBACK_ENV}=true)"
        );
    }

    // Build kernel after userland so version/toolchain metadata is available at compile time.
    let status = Command::new("cargo")
        .env("ARROST_BUILD_COUNT", &build_count_env)
        .env("ARROST_VERSION_MAJOR", &major_env)
        .env("ARROST_VERSION_MINOR", &minor_env)
        .env("ARROST_DOOM_APP", "doom")
        .env(
            "ARROST_DOOM_ARTIFACT_HINT",
            user_doom.hint.display().to_string(),
        )
        .env("ARROST_DOOM_ARTIFACT_SIZE", user_doom.size.to_string())
        .env(
            "ARROST_DOOM_C_BACKEND_OBJECT",
            doom_c_backend.object.display().to_string(),
        )
        .env(
            "ARROST_DOOM_C_BACKEND_SIZE",
            doom_c_backend.size.to_string(),
        )
        .env(
            "ARROST_DOOM_C_BACKEND_READY",
            if doom_c_backend.ready {
                "true"
            } else {
                "false"
            },
        )
        .env(
            "ARROST_DOOM_GENERIC_READY",
            if doom_generic_ready_for_kernel {
                "true"
            } else {
                "false"
            },
        )
        .env(
            "ARROST_DOOM_GENERIC_ROOT",
            doom_generic.root.display().to_string(),
        )
        .env(
            "ARROST_DOOM_GENERIC_CORE_SOURCE",
            doom_generic.core_source.display().to_string(),
        )
        .env(
            "ARROST_DOOM_GENERIC_CORE_OBJECT",
            doom_generic.core_object.display().to_string(),
        )
        .env(
            "ARROST_DOOM_GENERIC_CORE_SIZE",
            doom_generic.core_size.to_string(),
        )
        .env(
            "ARROST_DOOM_GENERIC_CORE_READY",
            if doom_generic.core_ready {
                "true"
            } else {
                "false"
            },
        )
        .env(
            "ARROST_DOOM_GENERIC_PORT_OBJECT",
            doom_generic.port_object.display().to_string(),
        )
        .env(
            "ARROST_DOOM_GENERIC_PORT_SIZE",
            doom_generic.port_size.to_string(),
        )
        .env(
            "ARROST_DOOM_GENERIC_PORT_READY",
            if doom_generic.port_ready {
                "true"
            } else {
                "false"
            },
        )
        .env(
            "ARROST_DOOM_WAD_HINT",
            doom_generic.wad_hint.display().to_string(),
        )
        .env(
            "ARROST_DOOM_WAD_PRESENT",
            if doom_generic.wad_present {
                "true"
            } else {
                "false"
            },
        )
        .args([
            "build",
            "-p",
            KERNEL_PACKAGE,
            "--target",
            KERNEL_TARGET,
            BUILD_STD,
            BUILD_STD_FEATURES,
        ])
        .status()
        .context("cargo build failed")?;
    if !status.success() {
        bail!("kernel build failed");
    }

    // Build a UEFI disk image using the host-side bootloader crate API.
    let kernel_binary = PathBuf::from(format!("target/{KERNEL_TARGET}/debug/{KERNEL_PACKAGE}"));
    if !kernel_binary.exists() {
        bail!("missing kernel binary at {}", kernel_binary.display());
    }
    let ramdisk_path =
        create_ramdisk_image(&user_init, &user_doom, &doom_c_backend, &doom_generic)?;
    let _storage_disk_path = ensure_storage_disk_image()?;

    let disk_image = PathBuf::from(format!(
        "target/{KERNEL_TARGET}/debug/bootimage-{KERNEL_PACKAGE}.bin"
    ));
    let mut builder = DiskImageBuilder::new(kernel_binary);
    builder.set_ramdisk(ramdisk_path);
    builder
        .create_uefi_image(&disk_image)
        .context("failed to create UEFI disk image")?;

    Ok(())
}

fn build_userland_package(
    package: &str,
    build_count_env: &str,
    major_env: &str,
    minor_env: &str,
) -> Result<UserArtifact> {
    let status = Command::new("cargo")
        .env("ARROST_BUILD_COUNT", build_count_env)
        .env("ARROST_VERSION_MAJOR", major_env)
        .env("ARROST_VERSION_MINOR", minor_env)
        .args([
            "build",
            "-p",
            package,
            "--target",
            KERNEL_TARGET,
            BUILD_STD,
            BUILD_STD_FEATURES,
        ])
        .status()
        .with_context(|| format!("cargo build for {package} failed"))?;
    if !status.success() {
        bail!("userland build failed for {package}");
    }

    let direct_hint = PathBuf::from(format!("target/{KERNEL_TARGET}/debug/{package}"));
    let lib_hint = PathBuf::from(format!(
        "target/{KERNEL_TARGET}/debug/lib{}.rlib",
        package.replace('-', "_")
    ));
    let hint = if direct_hint.exists() {
        direct_hint
    } else {
        lib_hint
    };
    let size = std::fs::metadata(&hint).map(|meta| meta.len()).unwrap_or(0);
    Ok(UserArtifact { hint, size })
}

fn build_doom_c_backend_artifact() -> Result<DoomCBackendArtifact> {
    let source = PathBuf::from(DOOM_C_SOURCE);
    if !source.exists() {
        bail!("missing doom C source at {}", source.display());
    }

    let object = PathBuf::from(format!("target/{KERNEL_TARGET}/debug/doom_backend.o"));
    if let Some(parent) = object.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let status = Command::new("cc")
        .args([
            "-std=c11",
            "-ffreestanding",
            "-fno-builtin",
            "-O2",
            "-Wall",
            "-Wextra",
            "-c",
        ])
        .arg(&source)
        .arg("-o")
        .arg(&object)
        .status();

    match status {
        Ok(status) if status.success() => {
            let size = std::fs::metadata(&object)
                .map(|meta| meta.len())
                .unwrap_or(0);
            Ok(DoomCBackendArtifact {
                object,
                size,
                ready: true,
            })
        }
        Ok(status) => {
            eprintln!(
                "warning: C backend compile exited with code {:?}; writing placeholder object",
                status.code()
            );
            std::fs::write(&object, b"ARR0ST_DOOM_C_BACKEND_UNAVAILABLE\n")
                .with_context(|| format!("failed to write {}", object.display()))?;
            let size = std::fs::metadata(&object)
                .map(|meta| meta.len())
                .unwrap_or(0);
            Ok(DoomCBackendArtifact {
                object,
                size,
                ready: false,
            })
        }
        Err(error) => {
            eprintln!(
                "warning: failed to execute C compiler ({error}); writing placeholder object"
            );
            std::fs::write(&object, b"ARR0ST_DOOM_C_BACKEND_UNAVAILABLE\n")
                .with_context(|| format!("failed to write {}", object.display()))?;
            let size = std::fs::metadata(&object)
                .map(|meta| meta.len())
                .unwrap_or(0);
            Ok(DoomCBackendArtifact {
                object,
                size,
                ready: false,
            })
        }
    }
}

fn build_doom_generic_artifact() -> Result<DoomGenericArtifact> {
    let root = PathBuf::from(DOOM_GENERIC_ROOT);
    let core_source = PathBuf::from(DOOM_GENERIC_CORE_SOURCE);
    let include_dir = PathBuf::from(DOOM_GENERIC_INCLUDE_DIR);
    let port_source = PathBuf::from(DOOM_GENERIC_PORT_SOURCE);
    let wad_hint = PathBuf::from(DOOM_WAD_HINT);
    let wad_present = wad_hint.exists();

    if !port_source.exists() {
        bail!(
            "missing doomgeneric port source at {}",
            port_source.display()
        );
    }

    let core_object = PathBuf::from(format!("target/{KERNEL_TARGET}/debug/doomgeneric_core.o"));
    if let Some(parent) = core_object.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let port_object = PathBuf::from(format!("target/{KERNEL_TARGET}/debug/doomgeneric_arrost.o"));
    if let Some(parent) = port_object.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let mut core_ready = false;
    if core_source.exists() {
        let status = Command::new("cc")
            .args([
                "-std=c11",
                "-ffreestanding",
                "-fno-builtin",
                "-O2",
                "-Wall",
                "-Wextra",
                "-c",
            ])
            .arg("-I")
            .arg(&include_dir)
            .arg(&core_source)
            .arg("-o")
            .arg(&core_object)
            .status();

        match status {
            Ok(status) if status.success() => {
                core_ready = true;
            }
            Ok(status) => {
                eprintln!(
                    "warning: doomgeneric core compile exited with code {:?}; writing placeholder object",
                    status.code()
                );
                std::fs::write(&core_object, b"ARR0ST_DOOMGENERIC_CORE_UNAVAILABLE\n")
                    .with_context(|| format!("failed to write {}", core_object.display()))?;
            }
            Err(error) => {
                eprintln!(
                    "warning: failed to execute C compiler for doomgeneric core ({error}); writing placeholder object"
                );
                std::fs::write(&core_object, b"ARR0ST_DOOMGENERIC_CORE_UNAVAILABLE\n")
                    .with_context(|| format!("failed to write {}", core_object.display()))?;
            }
        }
    } else {
        std::fs::write(&core_object, b"ARR0ST_DOOMGENERIC_CORE_MISSING\n")
            .with_context(|| format!("failed to write {}", core_object.display()))?;
    }

    let core_size = std::fs::metadata(&core_object)
        .map(|meta| meta.len())
        .unwrap_or(0);

    let mut port_ready = false;
    let status = Command::new("cc")
        .args([
            "-std=c11",
            "-ffreestanding",
            "-fno-builtin",
            "-O2",
            "-Wall",
            "-Wextra",
            "-c",
        ])
        .arg("-I")
        .arg(&include_dir)
        .arg(&port_source)
        .arg("-o")
        .arg(&port_object)
        .status();

    match status {
        Ok(status) if status.success() => {
            port_ready = true;
        }
        Ok(status) => {
            eprintln!(
                "warning: doomgeneric port compile exited with code {:?}; writing placeholder object",
                status.code()
            );
            std::fs::write(&port_object, b"ARR0ST_DOOMGENERIC_PORT_UNAVAILABLE\n")
                .with_context(|| format!("failed to write {}", port_object.display()))?;
        }
        Err(error) => {
            eprintln!(
                "warning: failed to execute C compiler for doomgeneric port ({error}); writing placeholder object"
            );
            std::fs::write(&port_object, b"ARR0ST_DOOMGENERIC_PORT_UNAVAILABLE\n")
                .with_context(|| format!("failed to write {}", port_object.display()))?;
        }
    }

    let port_size = std::fs::metadata(&port_object)
        .map(|meta| meta.len())
        .unwrap_or(0);

    if !core_source.exists() {
        eprintln!(
            "warning: missing DoomGeneric sources at {}; run scripts/vendor_doomgeneric.sh",
            root.display()
        );
    }
    if !wad_present {
        eprintln!(
            "warning: missing Doom WAD at {}; doom play will use fallback runtime",
            wad_hint.display()
        );
    }

    let ready = core_ready && port_ready && wad_present;
    Ok(DoomGenericArtifact {
        root,
        core_source,
        core_object,
        core_size,
        core_ready,
        port_object,
        port_size,
        port_ready,
        ready,
        wad_hint,
        wad_present,
    })
}

fn next_build_count() -> Result<u64> {
    let path = PathBuf::from(BUILD_COUNTER_FILE);
    let current = std::fs::read_to_string(&path)
        .ok()
        .and_then(|content| content.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let next = current.saturating_add(1);
    std::fs::write(&path, format!("{next}\n"))
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(next)
}

fn run_qemu() -> Result<()> {
    // Si appoggia a scripts/qemu.sh per semplicità
    let status = Command::new("bash")
        .args(["scripts/qemu.sh"])
        .status()
        .context("qemu run failed")?;
    if !status.success() {
        bail!("qemu exited with error");
    }
    Ok(())
}

fn smoke_doom() -> Result<()> {
    smoke_doom_impl(false, false, false)
}

fn smoke_doom_long() -> Result<()> {
    smoke_doom_impl(true, false, false)
}

fn smoke_doom_virtio() -> Result<()> {
    smoke_doom_impl(true, false, true)
}

fn smoke_doom_fallback() -> Result<()> {
    build_impl(true)?;
    let smoke_result = smoke_doom_impl(false, true, false);
    let restore_result = build_impl(false);
    match smoke_result {
        Ok(()) => {
            restore_result?;
            Ok(())
        }
        Err(smoke_err) => {
            if let Err(restore_err) = restore_result {
                return Err(smoke_err.context(format!(
                    "fallback smoke failed and restoring normal DoomGeneric build failed: {restore_err:#}"
                )));
            }
            Err(smoke_err)
        }
    }
}

fn smoke_doom_impl(long_run: bool, force_fallback: bool, strict_virtio: bool) -> Result<()> {
    let smoke_name = if strict_virtio {
        "smoke-doom-virtio"
    } else if force_fallback {
        "smoke-doom-fallback"
    } else if long_run {
        "smoke-doom-long"
    } else {
        "smoke-doom"
    };

    let kernel_image = PathBuf::from(format!(
        "target/{KERNEL_TARGET}/debug/bootimage-{KERNEL_PACKAGE}.bin"
    ));
    if !kernel_image.exists() {
        bail!(
            "missing kernel image at {}; run `cargo xtask build` first",
            kernel_image.display()
        );
    }

    let data_image = PathBuf::from(format!("target/{KERNEL_TARGET}/debug/m6-disk.img"));
    if !data_image.exists() {
        bail!(
            "missing storage image at {}; run `cargo xtask build` first",
            data_image.display()
        );
    }

    let mut qemu_cmd = Command::new("bash");
    qemu_cmd
        .args(["scripts/qemu.sh"])
        .env("QEMU_DISPLAY", "none");
    if strict_virtio {
        qemu_cmd.env("QEMU_VIRTIO_SND", "on");
        qemu_cmd.env("QEMU_PCSPK", "off");
    }
    if std::env::var_os("QEMU_AUDIO").is_none() {
        qemu_cmd.env("QEMU_AUDIO", "wav");
    }
    if std::env::var_os("QEMU_AUDIO_WAV_PATH").is_none() {
        qemu_cmd.env(
            "QEMU_AUDIO_WAV_PATH",
            format!("target/{KERNEL_TARGET}/debug/{smoke_name}.wav"),
        );
    }
    let mut child = qemu_cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to start qemu run for {smoke_name}"))?;

    let stdout = child
        .stdout
        .take()
        .context("failed to capture qemu stdout")?;
    let stderr = child
        .stderr
        .take()
        .context("failed to capture qemu stderr")?;

    let log = Arc::new(Mutex::new(Vec::<u8>::new()));
    let stdout_reader = spawn_log_reader(stdout, Arc::clone(&log));
    let stderr_reader = spawn_log_reader(stderr, Arc::clone(&log));

    let smoke_result = (|| -> Result<()> {
        wait_for_log(&log, "arrost> ", Duration::from_secs(40), "shell prompt")?;
        let stdin = child
            .stdin
            .as_mut()
            .context("failed to capture qemu stdin")?;

        send_serial_command(stdin, "versionx\u{7f}\n")?;
        wait_for_log(
            &log,
            "version: ",
            Duration::from_secs(8),
            "serial backspace",
        )?;

        let ready = snapshot_log(&log).contains("DoomGeneric: ready=true");
        if force_fallback && ready {
            bail!("expected DoomGeneric ready=false for fallback smoke");
        }
        if !force_fallback && !ready {
            bail!(
                "doomgeneric ready=false in smoke-doom; run `cargo xtask build` (or wait for fallback restore) and retry"
            );
        }

        send_serial_command(stdin, "doom play\n")?;
        let play_marker = if ready {
            "doom: play mode started (doomgeneric)"
        } else {
            "doom: doomgeneric not ready; starting fallback runtime"
        };
        wait_for_log(
            &log,
            play_marker,
            Duration::from_secs(12),
            "doom play confirmation",
        )?;

        if force_fallback {
            wait_for_log(
                &log,
                "doom: capture unavailable (fallback mode)",
                Duration::from_secs(8),
                "doom fallback capture notice",
            )?;

            send_serial_command(stdin, "doom status\n")?;
            wait_for_log(
                &log,
                "doom: app=doom engine=fallback-sim",
                Duration::from_secs(8),
                "doom fallback status line",
            )?;
            let fallback_snapshot = snapshot_log(&log);
            let Some(fallback_line) =
                last_matching_line(&fallback_snapshot, "doom: app=doom engine=fallback-sim")
            else {
                bail!("missing fallback status line");
            };
            if !fallback_line.contains("doomgeneric=false") {
                bail!("fallback status mismatch: expected doomgeneric=false");
            }

            send_serial_command(stdin, "doom key left\n")?;
            wait_for_log(
                &log,
                "doom: injected key 0x61",
                Duration::from_secs(8),
                "fallback key injection",
            )?;

            send_serial_command(stdin, "doom status\n")?;
            wait_for_log(
                &log,
                "doom: app=doom engine=fallback-sim",
                Duration::from_secs(8),
                "fallback status post-input",
            )?;

            send_serial_command(stdin, "ui\n")?;
            wait_for_log(
                &log,
                "ui: backend=uefi-gop ready=true",
                Duration::from_secs(8),
                "ui diagnostics line",
            )?;
            let ui_snapshot = snapshot_log(&log);
            if let Some(ui_line) =
                last_matching_line(&ui_snapshot, "ui: backend=uefi-gop ready=true")
                && let Some(stdout_dropped) = parse_metric_value(ui_line, "stdout_dropped=")
                && stdout_dropped > 0
            {
                bail!(
                    "stdout mirror dropped bytes during fallback smoke (stdout_dropped={stdout_dropped})"
                );
            }

            send_serial_command(stdin, "doom stop\n")?;
            wait_for_log(
                &log,
                "doom: runtime stopped",
                Duration::from_secs(8),
                "doom stop confirmation",
            )?;
            return Ok(());
        }

        if ready {
            wait_for_log(
                &log,
                "doom: capture enabled (press ESC to exit)",
                Duration::from_secs(8),
                "doom auto-capture enabled",
            )?;
            send_serial_command(stdin, "\u{1b}")?;
            wait_for_log(
                &log,
                "doom: capture disabled",
                Duration::from_secs(8),
                "doom auto-capture escape",
            )?;
        }

        wait_for_music_pcm_activity(&log, stdin, Duration::from_secs(10))?;

        send_serial_command(stdin, "doom audio off\n")?;
        wait_for_log(
            &log,
            "doom: audio mode set to off",
            Duration::from_secs(8),
            "doom audio off",
        )?;

        send_serial_command(stdin, "doom audio on\n")?;
        wait_for_log(
            &log,
            "doom: audio mode set to ",
            Duration::from_secs(8),
            "doom audio on",
        )?;

        if !long_run {
            send_serial_command(stdin, "doom audio test\n")?;
            wait_for_log(
                &log,
                "doom: audio test tone queued",
                Duration::from_secs(8),
                "doom audio test",
            )?;
        }

        send_serial_command(stdin, "doom capture on\n")?;
        wait_for_log(
            &log,
            "doom: capture enabled (press ESC to exit)",
            Duration::from_secs(8),
            "doom capture on",
        )?;

        send_serial_command(stdin, "ww  ww")?;
        thread::sleep(Duration::from_millis(220));

        send_serial_command(stdin, "\u{1b}")?;
        wait_for_log(
            &log,
            "doom: capture disabled",
            Duration::from_secs(8),
            "doom capture escape",
        )?;

        send_serial_command(stdin, "doom status\n")?;
        wait_for_log(
            &log,
            "doom: app=doom engine=",
            Duration::from_secs(8),
            "doom status post-capture",
        )?;
        let capture_snapshot = snapshot_log(&log);
        let Some(capture_status_line) =
            last_matching_line(&capture_snapshot, "doom: app=doom engine=")
        else {
            bail!("missing doom status line after serial capture input");
        };
        if !capture_status_line.contains("capture=false") {
            bail!("doom capture did not return to false after ESC");
        }
        let Some(capture_dg_key) = parse_metric_value(capture_status_line, "dg_key=") else {
            bail!("missing dg_key metric after serial capture input");
        };
        if capture_dg_key == 0 {
            bail!(
                "serial capture input did not produce bridge key events (dg_key={capture_dg_key})"
            );
        }

        send_serial_command(stdin, "doom mouse y on\n")?;
        wait_for_log(
            &log,
            "doom: mouse y mapping enabled",
            Duration::from_secs(8),
            "doom mouse y on",
        )?;

        send_serial_command(stdin, "doom mouse turn 5\n")?;
        wait_for_log(
            &log,
            "doom: mouse turn threshold set to 5",
            Duration::from_secs(8),
            "doom mouse turn",
        )?;

        send_serial_command(stdin, "doom mouse move 7\n")?;
        wait_for_log(
            &log,
            "doom: mouse move threshold set to 7",
            Duration::from_secs(8),
            "doom mouse move",
        )?;

        send_serial_command(stdin, "doom status\n")?;
        wait_for_log(
            &log,
            "doom: app=doom engine=",
            Duration::from_secs(8),
            "doom status line",
        )?;
        wait_for_log(
            &log,
            "mouse_cfg=(turn:5 move:7 y:true)",
            Duration::from_secs(8),
            "doom mouse config status",
        )?;

        send_serial_command(stdin, "doom key left\n")?;
        wait_for_log(
            &log,
            "doom: injected key 0x61",
            Duration::from_secs(8),
            "doom key injection",
        )?;

        send_serial_command(stdin, "doom keyup left\n")?;
        wait_for_log(
            &log,
            "doom: injected keyup 0x61",
            Duration::from_secs(8),
            "doom keyup injection",
        )?;

        send_serial_command(stdin, "doom key fire\n")?;
        wait_for_log(
            &log,
            "doom: injected key 0x20",
            Duration::from_secs(8),
            "doom fire injection",
        )?;
        thread::sleep(Duration::from_millis(220));

        send_serial_command(stdin, "doom keyup fire\n")?;
        wait_for_log(
            &log,
            "doom: injected keyup 0x20",
            Duration::from_secs(8),
            "doom fire keyup injection",
        )?;

        send_serial_command(stdin, "doom key enter\n")?;
        wait_for_log(
            &log,
            "doom: injected key 0x0a",
            Duration::from_secs(8),
            "doom enter injection",
        )?;

        send_serial_command(stdin, "doom keyup enter\n")?;
        wait_for_log(
            &log,
            "doom: injected keyup 0x0a",
            Duration::from_secs(8),
            "doom enter keyup injection",
        )?;

        send_serial_command(stdin, "doom status\n")?;
        wait_for_log(
            &log,
            "last_key=0x0a",
            Duration::from_secs(8),
            "doom status post-input",
        )?;
        let status_snapshot = snapshot_log(&log);
        let Some(status_line) = last_matching_line(&status_snapshot, "last_key=0x0a") else {
            bail!("missing doom status line after input injections");
        };
        let Some(inputs) = parse_metric_value(status_line, "inputs=") else {
            bail!("missing inputs metric in doom status line");
        };
        if inputs < 3 {
            bail!("unexpected low doom input count after injections (inputs={inputs})");
        }
        let Some(dg_frames) = parse_metric_value(status_line, "dg_frames=") else {
            bail!("missing dg_frames metric in doom status line");
        };
        if dg_frames < 2 {
            bail!("unexpected low doom frame count after play start (dg_frames={dg_frames})");
        }
        let Some(dg_key) = parse_metric_value(status_line, "dg_key=") else {
            bail!("missing dg_key metric in doom status line");
        };
        if dg_key == 0 {
            bail!("doom bridge did not register key queue events (dg_key={dg_key})");
        }
        let Some(dg_nonzero) = parse_metric_value(status_line, "dg_nonzero=") else {
            bail!("missing dg_nonzero metric in doom status line");
        };
        if dg_nonzero == 0 {
            bail!("doom frame appears fully black after play start (dg_nonzero={dg_nonzero})");
        }
        let Some(dg_audio) = parse_metric_value(status_line, "dg_audio=") else {
            bail!("missing dg_audio metric in doom status line");
        };
        if dg_audio == 0 {
            bail!("doom audio backend stub did not receive callbacks (dg_audio={dg_audio})");
        }
        let Some(dg_audio_samples) = parse_metric_value(status_line, "dg_audio_samples=") else {
            bail!("missing dg_audio_samples metric in doom status line");
        };
        let Some(pcm_samples) = parse_metric_value(status_line, "pcm_samples=") else {
            bail!("missing pcm_samples metric in doom status line");
        };
        if pcm_samples == 0 {
            bail!("pcm audio path inactive after play start (pcm_samples=0)");
        }
        let virtio_backend = status_line.contains("pcm_backend=virtio-snd");
        if strict_virtio && !virtio_backend {
            bail!("strict virtio smoke expected pcm_backend=virtio-snd");
        }
        if virtio_backend {
            let Some(pcm_tx) = parse_metric_value(status_line, "pcm_tx=") else {
                bail!("missing pcm_tx metric in virtio status line");
            };
            let Some(pcm_done) = parse_metric_value(status_line, "pcm_done=") else {
                bail!("missing pcm_done metric in virtio status line");
            };
            if pcm_tx == 0 || pcm_done == 0 {
                bail!("virtio-sound metrics inactive (pcm_tx={pcm_tx} pcm_done={pcm_done})");
            }
        } else {
            let Some(pcm_sw) = parse_metric_value(status_line, "pcm_sw=") else {
                bail!("missing pcm_sw metric in doom status line");
            };
            let Some(pcm_min) = parse_metric_value(status_line, "pcm_min=") else {
                bail!("missing pcm_min metric in doom status line");
            };
            let Some(pcm_max) = parse_metric_value(status_line, "pcm_max=") else {
                bail!("missing pcm_max metric in doom status line");
            };
            if pcm_min == 0 || pcm_max == 0 || pcm_max < pcm_min {
                bail!("invalid pcm frequency window (pcm_min={pcm_min} pcm_max={pcm_max})");
            }
            if pcm_samples >= 2048 && pcm_sw == 0 && pcm_max == pcm_min {
                bail!(
                    "pcm tone appears fixed after play start (pcm_sw={pcm_sw} pcm_min={pcm_min} pcm_max={pcm_max})"
                );
            }
        }
        // `doom audio test` injects explicit diagnostic PCM outside the Doom mixer path.
        // Skip it for long-run smokes and only account for it in short runs.
        let audio_test_pcm_budget = if long_run { 0u64 } else { 4096u64 };
        if dg_audio_samples > 0
            && pcm_samples
                > dg_audio_samples
                    .saturating_mul(4)
                    .saturating_add(audio_test_pcm_budget)
        {
            bail!(
                "unexpected pcm sample growth (pcm_samples={pcm_samples} dg_audio_samples={dg_audio_samples})"
            );
        }
        let dg_frames_before_progress = dg_frames;

        send_serial_command(stdin, "doom key right\n")?;
        wait_for_log(
            &log,
            "doom: injected key 0x64",
            Duration::from_secs(8),
            "doom right injection",
        )?;
        send_serial_command(stdin, "doom keyup right\n")?;
        wait_for_log(
            &log,
            "doom: injected keyup 0x64",
            Duration::from_secs(8),
            "doom right keyup injection",
        )?;

        send_serial_command(stdin, "doom status\n")?;
        wait_for_log(
            &log,
            "last_key=0x64",
            Duration::from_secs(8),
            "doom status frame progression",
        )?;
        let progression_snapshot = snapshot_log(&log);
        let Some(progression_line) = last_matching_line(&progression_snapshot, "last_key=0x64")
        else {
            bail!("missing doom status line for frame progression check");
        };
        let Some(dg_frames_after_progress) = parse_metric_value(progression_line, "dg_frames=")
        else {
            bail!("missing dg_frames metric in progression status line");
        };
        if dg_frames_after_progress <= dg_frames_before_progress {
            bail!(
                "doom frame counter did not progress (before={dg_frames_before_progress} after={dg_frames_after_progress})"
            );
        }
        let Some(dg_nonzero_after_progress) = parse_metric_value(progression_line, "dg_nonzero=")
        else {
            bail!("missing dg_nonzero metric in progression status line");
        };
        if dg_nonzero_after_progress == 0 {
            bail!("doom progression frame is fully black (dg_nonzero={dg_nonzero_after_progress})");
        }
        let Some(dg_drop_before_long) = parse_metric_value(progression_line, "dg_drop=") else {
            bail!("missing dg_drop metric in progression status line");
        };
        let Some(dg_audio_before_long) = parse_metric_value(progression_line, "dg_audio=") else {
            bail!("missing dg_audio metric in progression status line");
        };
        let pcm_drop_frames_before_long = if virtio_backend {
            let Some(value) = parse_metric_value(progression_line, "pcm_drop_frames=") else {
                bail!("missing pcm_drop_frames metric in progression status line");
            };
            Some(value)
        } else {
            None
        };
        let pcm_done_before_long = if virtio_backend {
            let Some(value) = parse_metric_value(progression_line, "pcm_done=") else {
                bail!("missing pcm_done metric in progression status line");
            };
            Some(value)
        } else {
            None
        };

        if long_run {
            let long_wait = Duration::from_secs(24);
            let min_frame_progress = 180u64;
            let max_drop_delta = 4u64;

            thread::sleep(long_wait);
            send_serial_command(stdin, "doom status\n")?;
            let long_line = wait_for_status_with_frame_progress(
                &log,
                dg_frames_after_progress,
                Duration::from_secs(8),
                "doom status long-run",
            )?;
            let Some(dg_frames_long) = parse_metric_value(&long_line, "dg_frames=") else {
                bail!("missing dg_frames metric in long-run status line");
            };
            if dg_frames_long <= dg_frames_after_progress {
                bail!(
                    "doom frame counter did not progress during long-run (before={dg_frames_after_progress} after={dg_frames_long})"
                );
            }
            let frame_delta = dg_frames_long - dg_frames_after_progress;
            if frame_delta < min_frame_progress {
                bail!(
                    "doom frame progression too low during long-run (delta={frame_delta}, waited={}s, min={min_frame_progress})",
                    long_wait.as_secs()
                );
            }

            let Some(dg_drop_long) = parse_metric_value(&long_line, "dg_drop=") else {
                bail!("missing dg_drop metric in long-run status line");
            };
            let drop_delta = dg_drop_long.saturating_sub(dg_drop_before_long);
            if drop_delta > max_drop_delta {
                bail!(
                    "doom key queue drops grew too much during long-run (delta={drop_delta}, max={max_drop_delta})"
                );
            }

            let Some(dg_nonzero_long) = parse_metric_value(&long_line, "dg_nonzero=") else {
                bail!("missing dg_nonzero metric in long-run status line");
            };
            if dg_nonzero_long == 0 {
                bail!("doom long-run frame is fully black (dg_nonzero={dg_nonzero_long})");
            }

            let Some(dg_audio_long) = parse_metric_value(&long_line, "dg_audio=") else {
                bail!("missing dg_audio metric in long-run status line");
            };
            if dg_audio_long <= dg_audio_before_long {
                bail!(
                    "doom audio hook did not progress during long-run (before={dg_audio_before_long} after={dg_audio_long})"
                );
            }

            if virtio_backend {
                let Some(pcm_drop_frames_long) = parse_metric_value(&long_line, "pcm_drop_frames=")
                else {
                    bail!("missing pcm_drop_frames metric in long-run status line");
                };
                let drop_frames_delta =
                    pcm_drop_frames_long.saturating_sub(pcm_drop_frames_before_long.unwrap_or(0));
                let max_drop_frames_delta = if strict_virtio { 512u64 } else { 1536u64 };
                if drop_frames_delta > max_drop_frames_delta {
                    bail!(
                        "virtio pcm_drop_frames grew too much during long-run (delta={drop_frames_delta}, max={max_drop_frames_delta})"
                    );
                }

                let Some(pcm_done_long) = parse_metric_value(&long_line, "pcm_done=") else {
                    bail!("missing pcm_done metric in long-run status line");
                };
                let done_delta = pcm_done_long.saturating_sub(pcm_done_before_long.unwrap_or(0));
                if done_delta == 0 {
                    bail!("virtio completion counter did not progress during long-run (pcm_done)");
                }
            }
        }

        send_serial_command(stdin, "ui\n")?;
        wait_for_log(
            &log,
            "ui: backend=uefi-gop ready=true",
            Duration::from_secs(8),
            "ui diagnostics line",
        )?;

        let log_snapshot = snapshot_log(&log);
        if let Some(ui_line) = last_matching_line(&log_snapshot, "ui: backend=uefi-gop ready=true")
            && let Some(stdout_dropped) = parse_metric_value(ui_line, "stdout_dropped=")
            && stdout_dropped > 0
        {
            bail!("stdout mirror dropped bytes during smoke run (stdout_dropped={stdout_dropped})");
        }

        send_serial_command(stdin, "doom stop\n")?;
        wait_for_log(
            &log,
            "doom: runtime stopped",
            Duration::from_secs(8),
            "doom stop confirmation",
        )?;

        Ok(())
    })();

    if child
        .try_wait()
        .context("failed to query qemu process status")?
        .is_none()
    {
        let _ = child.kill();
    }
    let _ = child.wait();
    let _ = stdout_reader.join();
    let _ = stderr_reader.join();

    let log_snapshot = snapshot_log(&log);
    if let Err(error) = smoke_result {
        eprintln!("{smoke_name} failed: {error}");
        eprintln!("----- serial tail -----");
        eprintln!("{}", log_tail(&log_snapshot, 80));
        return Err(error);
    }

    println!("{smoke_name}: PASS");
    if let Some(play_line) = last_matching_line(&log_snapshot, "doom: play mode started") {
        println!("{smoke_name}: {play_line}");
    }
    if let Some(audio_line) = last_matching_line(&log_snapshot, "Audio: backend=") {
        println!("{smoke_name}: {audio_line}");
    }
    if let Some(status_line) = last_matching_line(&log_snapshot, "doom: app=doom engine=") {
        println!("{smoke_name}: {status_line}");
    }
    if let Some(key_line) = last_matching_line(&log_snapshot, "doom: injected key 0x61") {
        println!("{smoke_name}: {key_line}");
    }
    if let Some(keyup_line) = last_matching_line(&log_snapshot, "doom: injected keyup 0x61") {
        println!("{smoke_name}: {keyup_line}");
    }
    if let Some(fire_line) = last_matching_line(&log_snapshot, "doom: injected key 0x20") {
        println!("{smoke_name}: {fire_line}");
    }
    if let Some(enter_line) = last_matching_line(&log_snapshot, "doom: injected key 0x0a") {
        println!("{smoke_name}: {enter_line}");
    }
    if let Some(ui_line) = last_matching_line(&log_snapshot, "ui: backend=uefi-gop ready=true") {
        println!("{smoke_name}: {ui_line}");
    }
    Ok(())
}

fn env_truthy(name: &str) -> bool {
    matches!(
        std::env::var(name).ok().as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on")
    )
}

fn send_serial_command(stdin: &mut ChildStdin, command: &str) -> Result<()> {
    stdin
        .write_all(command.as_bytes())
        .with_context(|| format!("failed to send command `{}`", command.trim_end()))?;
    stdin
        .flush()
        .with_context(|| format!("failed to flush command `{}`", command.trim_end()))?;
    Ok(())
}

fn wait_for_log(
    log: &Arc<Mutex<Vec<u8>>>,
    needle: &str,
    timeout: Duration,
    stage: &str,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let snapshot = snapshot_log(log);
        if snapshot.contains(needle) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("timeout waiting for {stage}: expected `{needle}`");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn wait_for_status_with_frame_progress(
    log: &Arc<Mutex<Vec<u8>>>,
    min_frames: u64,
    timeout: Duration,
    stage: &str,
) -> Result<String> {
    let deadline = Instant::now() + timeout;
    loop {
        let snapshot = snapshot_log(log);
        if let Some(line) = last_matching_line(&snapshot, "doom: app=doom engine=")
            && let Some(frames) = parse_metric_value(line, "dg_frames=")
            && frames > min_frames
        {
            return Ok(line.to_string());
        }
        if Instant::now() >= deadline {
            bail!("timeout waiting for {stage}: expected dg_frames>{min_frames}");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn wait_for_music_pcm_activity(
    log: &Arc<Mutex<Vec<u8>>>,
    stdin: &mut ChildStdin,
    timeout: Duration,
) -> Result<u64> {
    let deadline = Instant::now() + timeout;
    loop {
        send_serial_command(stdin, "doom audio status\n")?;
        wait_for_log(
            log,
            "doom: audio mode=",
            Duration::from_secs(8),
            "doom audio status",
        )?;
        let snapshot = snapshot_log(log);
        if let Some(line) = last_matching_line(&snapshot, "doom: audio mode=")
            && let Some(pcm_samples) = parse_metric_value(line, "pcm_samples=")
            && pcm_samples > 0
        {
            return Ok(pcm_samples);
        }
        if Instant::now() >= deadline {
            bail!("timeout waiting for music PCM activity before SFX (expected pcm_samples>0)");
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn spawn_log_reader<R: Read + Send + 'static>(
    mut reader: R,
    log: Arc<Mutex<Vec<u8>>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buffer = [0u8; 2048];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(len) => {
                    if let Ok(mut bytes) = log.lock() {
                        bytes.extend_from_slice(&buffer[..len]);
                    } else {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    })
}

fn snapshot_log(log: &Arc<Mutex<Vec<u8>>>) -> String {
    if let Ok(bytes) = log.lock() {
        return String::from_utf8_lossy(&bytes).into_owned();
    }
    String::new()
}

fn last_matching_line<'a>(log: &'a str, marker: &str) -> Option<&'a str> {
    log.lines().rev().find(|line| line.contains(marker))
}

fn parse_metric_value(line: &str, key: &str) -> Option<u64> {
    let start = line.find(key)?;
    let rest = &line[start + key.len()..];
    let value = rest
        .split(|ch: char| ch.is_whitespace() || ch == ',')
        .next()?;
    value.parse::<u64>().ok()
}

fn log_tail(log: &str, lines: usize) -> String {
    let mut tail = Vec::new();
    for line in log.lines().rev().take(lines) {
        tail.push(line);
    }
    tail.reverse();
    tail.join("\n")
}

fn create_ramdisk_image(
    user_init: &UserArtifact,
    user_doom: &UserArtifact,
    doom_c_backend: &DoomCBackendArtifact,
    doom_generic: &DoomGenericArtifact,
) -> Result<PathBuf> {
    let ramdisk_path = PathBuf::from(format!("target/{KERNEL_TARGET}/debug/ramdisk"));
    let payload = format!(
        "ARR0ST_INITRAMFS_V4\ninit_app=init\ninit_artifact_hint={}\ninit_artifact_size={}\ndoom_app=doom\ndoom_artifact_hint={}\ndoom_artifact_size={}\ndoom_c_backend_object={}\ndoom_c_backend_size={}\ndoom_c_backend_ready={}\ndoom_generic_root={}\ndoom_generic_core_source={}\ndoom_generic_core_object={}\ndoom_generic_core_size={}\ndoom_generic_core_ready={}\ndoom_generic_port_object={}\ndoom_generic_port_size={}\ndoom_generic_port_ready={}\ndoom_generic_ready={}\ndoom_wad_hint={}\ndoom_wad_present={}\n",
        user_init.hint.display(),
        user_init.size,
        user_doom.hint.display(),
        user_doom.size,
        doom_c_backend.object.display(),
        doom_c_backend.size,
        doom_c_backend.ready,
        doom_generic.root.display(),
        doom_generic.core_source.display(),
        doom_generic.core_object.display(),
        doom_generic.core_size,
        doom_generic.core_ready,
        doom_generic.port_object.display(),
        doom_generic.port_size,
        doom_generic.port_ready,
        doom_generic.ready,
        doom_generic.wad_hint.display(),
        doom_generic.wad_present
    );
    std::fs::write(&ramdisk_path, payload.as_bytes())
        .with_context(|| format!("failed to write {}", ramdisk_path.display()))?;
    Ok(ramdisk_path)
}

fn ensure_storage_disk_image() -> Result<PathBuf> {
    let disk_path = PathBuf::from(format!("target/{KERNEL_TARGET}/debug/m6-disk.img"));
    if disk_path.exists() {
        return Ok(disk_path);
    }

    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&disk_path)
        .with_context(|| format!("failed to create {}", disk_path.display()))?;
    file.set_len(M6_DISK_SIZE_BYTES)
        .with_context(|| format!("failed to size {}", disk_path.display()))?;
    Ok(disk_path)
}

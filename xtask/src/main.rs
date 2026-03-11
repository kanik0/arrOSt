use anyhow::{Context, Result, bail};
use arrost_user_doom as user_doom;
use arrost_user_init as user_init;
use arrostd::syscall::{self as abi_syscall, errno as abi_errno, shim as abi_shim};
use bootloader::DiskImageBuilder;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const KERNEL_TARGET: &str = "x86_64-unknown-none";
const KERNEL_TARGET_AARCH64: &str = "aarch64-unknown-none";
const UEFI_TARGET_AARCH64: &str = "aarch64-unknown-uefi";
const KERNEL_PACKAGE: &str = "arrost-kernel";
const AARCH64_UEFI_LOADER_PACKAGE: &str = "arrost-aarch64-uefi-loader";
const USER_INIT_PACKAGE: &str = "arrost-user-init";
const USER_DOOM_PACKAGE: &str = "arrost-user-doom";
const USER_INIT_RING3_BIN: &str = "ring3_init";
const USER_DOOM_RING3_BIN: &str = "ring3_doom";
const USER_BIN_LS: &str = "ls";
const USER_BIN_CAT: &str = "cat";
const USER_BIN_PS: &str = "ps";
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
const FS_TRACE_DENTRY_ENV: &str = "ARROST_FS_TRACE_DENTRY";
const RING3_BOOT_SMOKE_ENV: &str = "ARROST_RING3_BOOT_SMOKE";
const RING3_BOOT_SMOKE_FAULT_ENV: &str = "ARROST_RING3_BOOT_SMOKE_FAULT";
const RING3_ELF_GROUNDWORK_ENV: &str = "ARROST_RING3_ELF_GROUNDWORK";
const USER_INIT_ELF_HINT_ENV: &str = "ARROST_USER_INIT_ELF_HINT";
const USER_INIT_ELF_PRESENT_ENV: &str = "ARROST_USER_INIT_ELF_PRESENT";
const USER_DOOM_ELF_HINT_ENV: &str = "ARROST_USER_DOOM_ELF_HINT";
const USER_DOOM_ELF_PRESENT_ENV: &str = "ARROST_USER_DOOM_ELF_PRESENT";
const USER_BIN_LS_ELF_HINT_ENV: &str = "ARROST_USER_BIN_LS_ELF_HINT";
const USER_BIN_LS_ELF_PRESENT_ENV: &str = "ARROST_USER_BIN_LS_ELF_PRESENT";
const USER_BIN_CAT_ELF_HINT_ENV: &str = "ARROST_USER_BIN_CAT_ELF_HINT";
const USER_BIN_CAT_ELF_PRESENT_ENV: &str = "ARROST_USER_BIN_CAT_ELF_PRESENT";
const USER_BIN_PS_ELF_HINT_ENV: &str = "ARROST_USER_BIN_PS_ELF_HINT";
const USER_BIN_PS_ELF_PRESENT_ENV: &str = "ARROST_USER_BIN_PS_ELF_PRESENT";
const QEMU_SCRIPT_X86_64: &str = "scripts/qemu.sh";
const QEMU_SCRIPT_AARCH64: &str = "scripts/qemu-aarch64.sh";
const XTASK_USAGE: &str = "Usage: cargo xtask <build|abi-check [--arch <x86_64|aarch64>]...|run [--arch <x86_64|aarch64>]|smoke-doom [--arch <x86_64|aarch64>]|smoke-doom-long [--arch <x86_64|aarch64>]|smoke-doom-virtio [--arch <x86_64|aarch64>]|smoke-doom-fallback [--arch <x86_64|aarch64>]|smoke-proc-caps [--arch <x86_64|aarch64>]|smoke-proc-spawn [--arch <x86_64|aarch64>]|smoke-bin-exec [--arch <x86_64|aarch64>]|smoke-fs [--arch <x86_64|aarch64>]|smoke-ring3 [--arch <x86_64|aarch64>]|smoke-ring3-run [--arch <x86_64|aarch64>]|smoke-ring3-fault [--arch <aarch64>]|smoke-kpti-m11|smoke-net [--arch <x86_64|aarch64>]>";

#[derive(Clone, Copy, Eq, PartialEq)]
enum RuntimeArch {
    X86_64,
    Aarch64,
}

impl RuntimeArch {
    fn as_str(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::Aarch64 => "aarch64",
        }
    }

    fn kernel_target(self) -> &'static str {
        match self {
            Self::X86_64 => KERNEL_TARGET,
            Self::Aarch64 => KERNEL_TARGET_AARCH64,
        }
    }

    fn qemu_script(self) -> &'static str {
        match self {
            Self::X86_64 => QEMU_SCRIPT_X86_64,
            Self::Aarch64 => QEMU_SCRIPT_AARCH64,
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum TopLevelCommand {
    Help,
    Build,
    AbiCheck,
    Run,
    SmokeDoom,
    SmokeDoomLong,
    SmokeDoomVirtio,
    SmokeDoomFallback,
    SmokeProcCaps,
    SmokeProcSpawn,
    SmokeBinExec,
    SmokeFs,
    SmokeRing3,
    SmokeRing3Run,
    SmokeRing3Fault,
    SmokeKptiM11,
    SmokeNet,
}

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

#[derive(Clone, Copy)]
struct BuildVersionEnv<'a> {
    build_count: &'a str,
    major: &'a str,
    minor: &'a str,
}

#[derive(Clone, Copy)]
struct SecondaryBuildOptions {
    force_fallback: bool,
    ring3_boot_smoke: bool,
    ring3_boot_smoke_fault: bool,
    ring3_elf_groundwork: bool,
    trace_dentry: bool,
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let top_level = parse_top_level_command(args.next().as_deref());
    match top_level {
        Ok(TopLevelCommand::Help) => {
            print_usage();
            Ok(())
        }
        Ok(TopLevelCommand::Build) => build(),
        Ok(TopLevelCommand::AbiCheck) => abi_check(parse_abi_check_arch_args(args)?),
        Ok(TopLevelCommand::Run) => run_qemu(parse_run_arch_arg(args)?),
        Ok(TopLevelCommand::SmokeDoom) => smoke_doom(parse_run_arch_arg(args)?),
        Ok(TopLevelCommand::SmokeDoomLong) => smoke_doom_long(parse_run_arch_arg(args)?),
        Ok(TopLevelCommand::SmokeDoomVirtio) => smoke_doom_virtio(parse_run_arch_arg(args)?),
        Ok(TopLevelCommand::SmokeDoomFallback) => smoke_doom_fallback(parse_run_arch_arg(args)?),
        Ok(TopLevelCommand::SmokeProcCaps) => smoke_proc_caps(parse_run_arch_arg(args)?),
        Ok(TopLevelCommand::SmokeProcSpawn) => smoke_proc_spawn(parse_run_arch_arg(args)?),
        Ok(TopLevelCommand::SmokeBinExec) => smoke_bin_exec(parse_run_arch_arg(args)?),
        Ok(TopLevelCommand::SmokeFs) => smoke_fs(parse_run_arch_arg(args)?),
        Ok(TopLevelCommand::SmokeRing3) => smoke_ring3(parse_run_arch_arg(args)?),
        Ok(TopLevelCommand::SmokeRing3Run) => smoke_ring3_run(parse_run_arch_arg(args)?),
        Ok(TopLevelCommand::SmokeRing3Fault) => smoke_ring3_fault(parse_run_arch_arg(args)?),
        Ok(TopLevelCommand::SmokeKptiM11) => smoke_kpti_m11(),
        Ok(TopLevelCommand::SmokeNet) => smoke_net(parse_run_arch_arg(args)?),
        Err(error) => {
            print_usage();
            Err(error)
        }
    }
}

fn print_usage() {
    eprintln!("{XTASK_USAGE}");
}

fn parse_top_level_command(value: Option<&str>) -> Result<TopLevelCommand> {
    match value {
        None | Some("help") | Some("-h") | Some("--help") => Ok(TopLevelCommand::Help),
        Some("build") => Ok(TopLevelCommand::Build),
        Some("abi-check") => Ok(TopLevelCommand::AbiCheck),
        Some("run") => Ok(TopLevelCommand::Run),
        Some("smoke-doom") => Ok(TopLevelCommand::SmokeDoom),
        Some("smoke-doom-long") => Ok(TopLevelCommand::SmokeDoomLong),
        Some("smoke-doom-virtio") => Ok(TopLevelCommand::SmokeDoomVirtio),
        Some("smoke-doom-fallback") => Ok(TopLevelCommand::SmokeDoomFallback),
        Some("smoke-proc-caps") => Ok(TopLevelCommand::SmokeProcCaps),
        Some("smoke-proc-spawn") => Ok(TopLevelCommand::SmokeProcSpawn),
        Some("smoke-bin-exec") => Ok(TopLevelCommand::SmokeBinExec),
        Some("smoke-fs") => Ok(TopLevelCommand::SmokeFs),
        Some("smoke-ring3") => Ok(TopLevelCommand::SmokeRing3),
        Some("smoke-ring3-run") => Ok(TopLevelCommand::SmokeRing3Run),
        Some("smoke-ring3-fault") => Ok(TopLevelCommand::SmokeRing3Fault),
        Some("smoke-kpti-m11") => Ok(TopLevelCommand::SmokeKptiM11),
        Some("smoke-net") => Ok(TopLevelCommand::SmokeNet),
        Some(other) => bail!("unsupported xtask command: {other}"),
    }
}

fn abi_check(arch_targets: Vec<RuntimeArch>) -> Result<()> {
    run_cargo_checked(&["test", "-p", "arrostd"], "arrostd ABI tests")?;
    run_cargo_checked(&["test", "-p", "arrost-user-init"], "user init ABI tests")?;
    run_cargo_checked(&["test", "-p", "arrost-user-doom"], "user doom ABI tests")?;

    for arch in arch_targets {
        let started = Instant::now();
        let packages = run_target_abi_build_checks(arch)?;
        println!(
            "abi-check: target={} packages={} status=ok elapsed_ms={}",
            arch.as_str(),
            packages,
            started.elapsed().as_millis()
        );
    }

    println!("abi-check: PASS");
    Ok(())
}

fn run_cargo_checked(args: &[&str], stage: &str) -> Result<()> {
    let status = Command::new("cargo")
        .args(args)
        .status()
        .with_context(|| format!("failed to run cargo for {stage}"))?;
    if !status.success() {
        bail!("{stage} failed");
    }
    Ok(())
}

fn run_target_abi_build_checks(arch: RuntimeArch) -> Result<usize> {
    let target = arch.kernel_target();
    let arch_name = arch.as_str();
    let packages = [USER_INIT_PACKAGE, USER_DOOM_PACKAGE, KERNEL_PACKAGE];
    let mut checked = 0usize;
    for package in packages {
        let stage = format!("{package} ABI build check ({arch_name})");
        let status = Command::new("cargo")
            .args([
                "build",
                "-p",
                package,
                "--target",
                target,
                BUILD_STD,
                BUILD_STD_FEATURES,
            ])
            .status()
            .with_context(|| format!("failed to run cargo for {stage}"))?;
        if !status.success() {
            bail!("{stage} failed");
        }
        checked = checked.saturating_add(1);
    }
    Ok(checked)
}

fn parse_abi_check_arch_args(args: impl Iterator<Item = String>) -> Result<Vec<RuntimeArch>> {
    let mut parsed = Vec::<RuntimeArch>::new();
    let mut iter = args.peekable();
    while let Some(arg) = iter.next() {
        let value = match arg.as_str() {
            "--arch" => iter
                .next()
                .context("missing value for --arch (expected x86_64 or aarch64)")?,
            _ => {
                if let Some(value) = arg.strip_prefix("--arch=") {
                    if value.is_empty() {
                        bail!("missing value for --arch= (expected x86_64 or aarch64)");
                    }
                    value.to_string()
                } else {
                    bail!("unsupported argument: {arg} (supported: --arch <x86_64|aarch64>)");
                }
            }
        };
        parsed.push(resolve_runtime_arch(Some(value))?);
    }

    if parsed.is_empty() {
        return Ok(vec![RuntimeArch::X86_64, RuntimeArch::Aarch64]);
    }

    let mut dedup = Vec::<RuntimeArch>::new();
    for arch in parsed {
        if !dedup.contains(&arch) {
            dedup.push(arch);
        }
    }
    Ok(dedup)
}

fn build() -> Result<()> {
    build_impl(
        env_truthy(DOOM_FORCE_FALLBACK_ENV),
        false,
        false,
        None,
        false,
    )
}

fn build_impl(
    force_fallback: bool,
    ring3_boot_smoke: bool,
    ring3_boot_smoke_fault: bool,
    ring3_elf_groundwork_override: Option<bool>,
    trace_dentry: bool,
) -> Result<()> {
    let build_count = next_build_count()?;
    let ring3_elf_groundwork =
        ring3_elf_groundwork_override.unwrap_or_else(default_ring3_elf_groundwork_enabled);
    let version = format!("{VERSION_MAJOR}.{VERSION_MINOR}.{build_count}");
    let build_count_env = build_count.to_string();
    let major_env = VERSION_MAJOR.to_string();
    let minor_env = VERSION_MINOR.to_string();
    println!("ArrOSt build version: {version}");

    let user_init_elf = build_userland_binary(
        USER_INIT_PACKAGE,
        USER_INIT_RING3_BIN,
        KERNEL_TARGET,
        &build_count_env,
        &major_env,
        &minor_env,
    )?;
    let user_doom = build_userland_package(
        USER_DOOM_PACKAGE,
        KERNEL_TARGET,
        &build_count_env,
        &major_env,
        &minor_env,
    )?;
    let user_doom_elf = build_userland_binary(
        USER_DOOM_PACKAGE,
        USER_DOOM_RING3_BIN,
        KERNEL_TARGET,
        &build_count_env,
        &major_env,
        &minor_env,
    )?;
    let user_bin_ls = build_userland_fs_binary(
        USER_INIT_PACKAGE,
        USER_BIN_LS,
        KERNEL_TARGET,
        &build_count_env,
        &major_env,
        &minor_env,
    )?;
    let user_bin_cat = build_userland_fs_binary(
        USER_INIT_PACKAGE,
        USER_BIN_CAT,
        KERNEL_TARGET,
        &build_count_env,
        &major_env,
        &minor_env,
    )?;
    let user_bin_ps = build_userland_fs_binary(
        USER_INIT_PACKAGE,
        USER_BIN_PS,
        KERNEL_TARGET,
        &build_count_env,
        &major_env,
        &minor_env,
    )?;
    let doom_c_backend = build_doom_c_backend_artifact(KERNEL_TARGET)?;
    let doom_generic = build_doom_generic_artifact(KERNEL_TARGET)?;
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
            USER_INIT_ELF_HINT_ENV,
            user_init_elf.hint.display().to_string(),
        )
        .env(
            USER_INIT_ELF_PRESENT_ENV,
            if user_init_elf.size > 0 {
                "true"
            } else {
                "false"
            },
        )
        .env(
            USER_DOOM_ELF_HINT_ENV,
            user_doom_elf.hint.display().to_string(),
        )
        .env(
            USER_DOOM_ELF_PRESENT_ENV,
            if user_doom_elf.size > 0 {
                "true"
            } else {
                "false"
            },
        )
        .env(
            USER_BIN_LS_ELF_HINT_ENV,
            user_bin_ls.hint.display().to_string(),
        )
        .env(
            USER_BIN_LS_ELF_PRESENT_ENV,
            if user_bin_ls.size > 0 {
                "true"
            } else {
                "false"
            },
        )
        .env(
            USER_BIN_CAT_ELF_HINT_ENV,
            user_bin_cat.hint.display().to_string(),
        )
        .env(
            USER_BIN_CAT_ELF_PRESENT_ENV,
            if user_bin_cat.size > 0 {
                "true"
            } else {
                "false"
            },
        )
        .env(
            USER_BIN_PS_ELF_HINT_ENV,
            user_bin_ps.hint.display().to_string(),
        )
        .env(
            USER_BIN_PS_ELF_PRESENT_ENV,
            if user_bin_ps.size > 0 {
                "true"
            } else {
                "false"
            },
        )
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
        .env(
            RING3_BOOT_SMOKE_ENV,
            if ring3_boot_smoke { "true" } else { "false" },
        )
        .env(
            RING3_BOOT_SMOKE_FAULT_ENV,
            if ring3_boot_smoke_fault {
                "true"
            } else {
                "false"
            },
        )
        .env(
            RING3_ELF_GROUNDWORK_ENV,
            if ring3_elf_groundwork {
                "true"
            } else {
                "false"
            },
        )
        .env(
            FS_TRACE_DENTRY_ENV,
            if trace_dentry { "true" } else { "false" },
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
        create_ramdisk_image(&user_init_elf, &user_doom, &doom_c_backend, &doom_generic)?;
    let _storage_disk_path = ensure_storage_disk_image()?;

    let disk_image = PathBuf::from(format!(
        "target/{KERNEL_TARGET}/debug/bootimage-{KERNEL_PACKAGE}.bin"
    ));
    let mut builder = DiskImageBuilder::new(kernel_binary);
    builder.set_ramdisk(ramdisk_path);
    builder
        .create_uefi_image(&disk_image)
        .context("failed to create UEFI disk image")?;

    build_secondary_target(
        BuildVersionEnv {
            build_count: &build_count_env,
            major: &major_env,
            minor: &minor_env,
        },
        SecondaryBuildOptions {
            force_fallback,
            ring3_boot_smoke,
            ring3_boot_smoke_fault,
            ring3_elf_groundwork,
            trace_dentry,
        },
    )?;

    Ok(())
}

fn build_secondary_target(
    version: BuildVersionEnv<'_>,
    options: SecondaryBuildOptions,
) -> Result<()> {
    println!("ArrOSt target build: {KERNEL_TARGET_AARCH64}");

    let user_init_elf = build_userland_binary(
        USER_INIT_PACKAGE,
        USER_INIT_RING3_BIN,
        KERNEL_TARGET_AARCH64,
        version.build_count,
        version.major,
        version.minor,
    )?;
    let user_doom = build_userland_package(
        USER_DOOM_PACKAGE,
        KERNEL_TARGET_AARCH64,
        version.build_count,
        version.major,
        version.minor,
    )?;
    let user_doom_elf = build_userland_binary(
        USER_DOOM_PACKAGE,
        USER_DOOM_RING3_BIN,
        KERNEL_TARGET_AARCH64,
        version.build_count,
        version.major,
        version.minor,
    )?;
    let user_bin_ls = build_userland_fs_binary(
        USER_INIT_PACKAGE,
        USER_BIN_LS,
        KERNEL_TARGET_AARCH64,
        version.build_count,
        version.major,
        version.minor,
    )?;
    let user_bin_cat = build_userland_fs_binary(
        USER_INIT_PACKAGE,
        USER_BIN_CAT,
        KERNEL_TARGET_AARCH64,
        version.build_count,
        version.major,
        version.minor,
    )?;
    let user_bin_ps = build_userland_fs_binary(
        USER_INIT_PACKAGE,
        USER_BIN_PS,
        KERNEL_TARGET_AARCH64,
        version.build_count,
        version.major,
        version.minor,
    )?;
    let doom_c_backend = build_doom_c_backend_artifact(KERNEL_TARGET_AARCH64)?;
    let doom_generic = build_doom_generic_artifact(KERNEL_TARGET_AARCH64)?;
    println!(
        "ArrOSt doom backend object ({KERNEL_TARGET_AARCH64}): ready={} path={} size={}",
        doom_c_backend.ready,
        doom_c_backend.object.display(),
        doom_c_backend.size
    );
    println!(
        "ArrOSt doomgeneric ({KERNEL_TARGET_AARCH64}): ready={} root={} core={} core_obj={} core_size={} core_ready={} port={} port_size={} port_ready={} wad={} wad_present={}",
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
    let doom_generic_ready_for_kernel = doom_generic.ready && !options.force_fallback;
    if options.force_fallback {
        println!(
            "ArrOSt doomgeneric ({KERNEL_TARGET_AARCH64}): forcing ready=false for kernel metadata ({DOOM_FORCE_FALLBACK_ENV}=true)"
        );
    }

    let status = Command::new("cargo")
        .env("ARROST_BUILD_COUNT", version.build_count)
        .env("ARROST_VERSION_MAJOR", version.major)
        .env("ARROST_VERSION_MINOR", version.minor)
        .env("ARROST_DOOM_APP", "doom")
        .env(
            "ARROST_DOOM_ARTIFACT_HINT",
            user_doom.hint.display().to_string(),
        )
        .env("ARROST_DOOM_ARTIFACT_SIZE", user_doom.size.to_string())
        .env(
            USER_INIT_ELF_HINT_ENV,
            user_init_elf.hint.display().to_string(),
        )
        .env(
            USER_INIT_ELF_PRESENT_ENV,
            if user_init_elf.size > 0 {
                "true"
            } else {
                "false"
            },
        )
        .env(
            USER_DOOM_ELF_HINT_ENV,
            user_doom_elf.hint.display().to_string(),
        )
        .env(
            USER_DOOM_ELF_PRESENT_ENV,
            if user_doom_elf.size > 0 {
                "true"
            } else {
                "false"
            },
        )
        .env(
            USER_BIN_LS_ELF_HINT_ENV,
            user_bin_ls.hint.display().to_string(),
        )
        .env(
            USER_BIN_LS_ELF_PRESENT_ENV,
            if user_bin_ls.size > 0 {
                "true"
            } else {
                "false"
            },
        )
        .env(
            USER_BIN_CAT_ELF_HINT_ENV,
            user_bin_cat.hint.display().to_string(),
        )
        .env(
            USER_BIN_CAT_ELF_PRESENT_ENV,
            if user_bin_cat.size > 0 {
                "true"
            } else {
                "false"
            },
        )
        .env(
            USER_BIN_PS_ELF_HINT_ENV,
            user_bin_ps.hint.display().to_string(),
        )
        .env(
            USER_BIN_PS_ELF_PRESENT_ENV,
            if user_bin_ps.size > 0 {
                "true"
            } else {
                "false"
            },
        )
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
        .env(
            RING3_BOOT_SMOKE_ENV,
            if options.ring3_boot_smoke {
                "true"
            } else {
                "false"
            },
        )
        .env(
            RING3_BOOT_SMOKE_FAULT_ENV,
            if options.ring3_boot_smoke_fault {
                "true"
            } else {
                "false"
            },
        )
        .env(
            RING3_ELF_GROUNDWORK_ENV,
            if options.ring3_elf_groundwork {
                "true"
            } else {
                "false"
            },
        )
        .env(
            FS_TRACE_DENTRY_ENV,
            if options.trace_dentry {
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
            KERNEL_TARGET_AARCH64,
            BUILD_STD,
            BUILD_STD_FEATURES,
        ])
        .status()
        .with_context(|| format!("cargo build failed for {KERNEL_TARGET_AARCH64}"))?;
    if !status.success() {
        bail!("kernel build failed ({KERNEL_TARGET_AARCH64})");
    }

    build_aarch64_uefi_payload()?;

    Ok(())
}

fn build_aarch64_uefi_payload() -> Result<()> {
    println!("ArrOSt target build: {UEFI_TARGET_AARCH64} (UEFI chain loader)");

    let status = Command::new("cargo")
        .env("RUSTFLAGS", "-C panic=abort")
        .args([
            "build",
            "-p",
            AARCH64_UEFI_LOADER_PACKAGE,
            "--target",
            UEFI_TARGET_AARCH64,
        ])
        .status()
        .with_context(|| format!("cargo build failed for {UEFI_TARGET_AARCH64}"))?;
    if !status.success() {
        bail!(
            "UEFI loader build failed ({UEFI_TARGET_AARCH64}); install target with `rustup target add {UEFI_TARGET_AARCH64}`"
        );
    }

    let loader_candidate_efi = PathBuf::from(format!(
        "target/{UEFI_TARGET_AARCH64}/debug/{AARCH64_UEFI_LOADER_PACKAGE}.efi"
    ));
    let loader_candidate_bin = PathBuf::from(format!(
        "target/{UEFI_TARGET_AARCH64}/debug/{AARCH64_UEFI_LOADER_PACKAGE}"
    ));
    let loader_binary = if loader_candidate_efi.exists() {
        loader_candidate_efi
    } else if loader_candidate_bin.exists() {
        loader_candidate_bin
    } else {
        bail!(
            "missing UEFI loader artifact at {} or {}",
            loader_candidate_efi.display(),
            loader_candidate_bin.display()
        );
    };

    let kernel_binary = PathBuf::from(format!(
        "target/{KERNEL_TARGET_AARCH64}/debug/{KERNEL_PACKAGE}"
    ));
    if !kernel_binary.exists() {
        bail!(
            "missing aarch64 kernel binary at {}",
            kernel_binary.display()
        );
    }

    let esp_root = PathBuf::from(format!("target/{KERNEL_TARGET_AARCH64}/debug/efi"));
    let esp_boot_dir = esp_root.join("EFI/BOOT");
    std::fs::create_dir_all(&esp_boot_dir).with_context(|| {
        format!(
            "failed to create aarch64 ESP boot directory {}",
            esp_boot_dir.display()
        )
    })?;

    let boot_efi_path = esp_boot_dir.join("BOOTAA64.EFI");
    std::fs::copy(&loader_binary, &boot_efi_path).with_context(|| {
        format!(
            "failed to stage UEFI loader {} -> {}",
            loader_binary.display(),
            boot_efi_path.display()
        )
    })?;

    let staged_kernel_path = esp_root.join("arrost-kernel");
    std::fs::copy(&kernel_binary, &staged_kernel_path).with_context(|| {
        format!(
            "failed to stage aarch64 kernel {} -> {}",
            kernel_binary.display(),
            staged_kernel_path.display()
        )
    })?;

    println!(
        "ArrOSt aarch64 UEFI payload: esp={} loader={} kernel={}",
        esp_root.display(),
        boot_efi_path.display(),
        staged_kernel_path.display()
    );

    Ok(())
}

fn build_userland_package(
    package: &str,
    target: &str,
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
            target,
            BUILD_STD,
            BUILD_STD_FEATURES,
        ])
        .status()
        .with_context(|| format!("cargo build for {package} ({target}) failed"))?;
    if !status.success() {
        bail!("userland build failed for {package} ({target})");
    }

    let direct_hint = PathBuf::from(format!("target/{target}/debug/{package}"));
    let lib_hint = PathBuf::from(format!(
        "target/{target}/debug/lib{}.rlib",
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

fn build_userland_binary(
    package: &str,
    binary: &str,
    target: &str,
    build_count_env: &str,
    major_env: &str,
    minor_env: &str,
) -> Result<UserArtifact> {
    let mut command = Command::new("cargo");
    command
        .env("ARROST_BUILD_COUNT", build_count_env)
        .env("ARROST_VERSION_MAJOR", major_env)
        .env("ARROST_VERSION_MINOR", minor_env);
    if let Some(rustflags) = userland_rustflags(target) {
        command.env("RUSTFLAGS", rustflags);
    }
    let status = command
        .args([
            "build",
            "-p",
            package,
            "--bin",
            binary,
            "--target",
            target,
            BUILD_STD,
            BUILD_STD_FEATURES,
        ])
        .status()
        .with_context(|| format!("cargo build for {package} bin {binary} ({target}) failed"))?;
    if !status.success() {
        bail!("user binary build failed for {package} bin {binary} ({target})");
    }

    let hint = PathBuf::from(format!("target/{target}/debug/{binary}"));
    let size = std::fs::metadata(&hint).map(|meta| meta.len()).unwrap_or(0);
    Ok(UserArtifact { hint, size })
}

fn build_userland_fs_binary(
    package: &str,
    binary: &str,
    target: &str,
    build_count_env: &str,
    major_env: &str,
    minor_env: &str,
) -> Result<UserArtifact> {
    let mut command = Command::new("cargo");
    command
        .env("ARROST_BUILD_COUNT", build_count_env)
        .env("ARROST_VERSION_MAJOR", major_env)
        .env("ARROST_VERSION_MINOR", minor_env);
    if let Some(rustflags) = userland_rustflags(target) {
        command.env("RUSTFLAGS", rustflags);
    }
    let status = command
        .args([
            "rustc",
            "-p",
            package,
            "--bin",
            binary,
            "--release",
            "--target",
            target,
            BUILD_STD,
            BUILD_STD_FEATURES,
            "--",
            "-C",
            "strip=debuginfo",
        ])
        .status()
        .with_context(|| format!("cargo rustc for {package} fs bin {binary} ({target}) failed"))?;
    if !status.success() {
        bail!("filesystem user binary build failed for {package} bin {binary} ({target})");
    }

    let hint = PathBuf::from(format!("target/{target}/release/{binary}"));
    let size = std::fs::metadata(&hint).map(|meta| meta.len()).unwrap_or(0);
    Ok(UserArtifact { hint, size })
}

fn userland_rustflags(target: &str) -> Option<&'static str> {
    if target == KERNEL_TARGET {
        Some("-C code-model=large")
    } else {
        None
    }
}

fn base_c_compile_command(_target: &str) -> Command {
    let mut command = Command::new("cc");
    command.args([
        "-std=c11",
        "-ffreestanding",
        "-fno-builtin",
        "-O2",
        "-Wall",
        "-Wextra",
        "-c",
    ]);
    command
}

fn build_doom_c_backend_artifact(target: &str) -> Result<DoomCBackendArtifact> {
    let source = PathBuf::from(DOOM_C_SOURCE);
    if !source.exists() {
        bail!("missing doom C source at {}", source.display());
    }

    let object = PathBuf::from(format!("target/{target}/debug/doom_backend.o"));
    if let Some(parent) = object.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let status = base_c_compile_command(target)
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

fn build_doom_generic_artifact(target: &str) -> Result<DoomGenericArtifact> {
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

    let core_object = PathBuf::from(format!("target/{target}/debug/doomgeneric_core.o"));
    if let Some(parent) = core_object.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let port_object = PathBuf::from(format!("target/{target}/debug/doomgeneric_arrost.o"));
    if let Some(parent) = port_object.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let mut core_ready = false;
    if core_source.exists() {
        let status = base_c_compile_command(target)
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
    let status = base_c_compile_command(target)
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

fn run_qemu(arch_override: Option<String>) -> Result<()> {
    let arch = resolve_runtime_arch(arch_override)?;

    let status = Command::new("bash")
        .args([arch.qemu_script()])
        .status()
        .context("qemu run failed")?;
    if !status.success() {
        bail!("qemu exited with error");
    }
    Ok(())
}

fn parse_run_arch_arg(args: impl Iterator<Item = String>) -> Result<Option<String>> {
    let mut arch = None;
    let mut iter = args.peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--arch" => {
                let value = iter
                    .next()
                    .context("missing value for --arch (expected x86_64 or aarch64)")?;
                if arch.is_some() {
                    bail!("duplicate --arch argument (supported: --arch <x86_64|aarch64> once)");
                }
                arch = Some(value);
            }
            _ => {
                if let Some(value) = arg.strip_prefix("--arch=") {
                    if value.is_empty() {
                        bail!("missing value for --arch= (expected x86_64 or aarch64)");
                    }
                    if arch.is_some() {
                        bail!(
                            "duplicate --arch argument (supported: --arch <x86_64|aarch64> once)"
                        );
                    }
                    arch = Some(value.to_string());
                } else {
                    bail!("unsupported argument: {arg} (supported: --arch <x86_64|aarch64>)");
                }
            }
        }
    }
    Ok(arch)
}

fn resolve_runtime_arch(arch_override: Option<String>) -> Result<RuntimeArch> {
    let arch_raw = arch_override
        .or_else(|| std::env::var("ARROST_ARCH").ok())
        .unwrap_or_else(|| RuntimeArch::X86_64.as_str().to_string());
    match arch_raw.as_str() {
        "x86_64" | "amd64" => Ok(RuntimeArch::X86_64),
        "aarch64" | "arm64" => Ok(RuntimeArch::Aarch64),
        other => bail!("unsupported arch={other}; expected x86_64 or aarch64"),
    }
}

fn ensure_runtime_artifacts(arch: RuntimeArch) -> Result<()> {
    match arch {
        RuntimeArch::X86_64 => {
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
        }
        RuntimeArch::Aarch64 => {
            let esp_dir = PathBuf::from(format!("target/{KERNEL_TARGET_AARCH64}/debug/efi"));
            let boot_efi = esp_dir.join("EFI/BOOT/BOOTAA64.EFI");
            let kernel_image = esp_dir.join("arrost-kernel");
            if !boot_efi.exists() {
                bail!(
                    "missing aarch64 UEFI loader at {}; run `cargo xtask build` first",
                    boot_efi.display()
                );
            }
            if !kernel_image.exists() {
                bail!(
                    "missing aarch64 staged kernel at {}; run `cargo xtask build` first",
                    kernel_image.display()
                );
            }

            let data_image = PathBuf::from(format!("target/{KERNEL_TARGET}/debug/m6-disk.img"));
            if !data_image.exists() {
                bail!(
                    "missing shared storage image at {}; run `cargo xtask build` first",
                    data_image.display()
                );
            }
        }
    }
    Ok(())
}

fn smoke_doom(arch_override: Option<String>) -> Result<()> {
    let arch = resolve_runtime_arch(arch_override)?;
    smoke_doom_impl(arch, false, false, false)
}

fn smoke_doom_long(arch_override: Option<String>) -> Result<()> {
    let arch = resolve_runtime_arch(arch_override)?;
    smoke_doom_impl(arch, true, false, false)
}

fn smoke_doom_virtio(arch_override: Option<String>) -> Result<()> {
    let arch = resolve_runtime_arch(arch_override)?;
    smoke_doom_impl(arch, true, false, true)
}

fn smoke_doom_fallback(arch_override: Option<String>) -> Result<()> {
    let arch = resolve_runtime_arch(arch_override)?;
    build_impl(true, false, false, None, false)?;
    let smoke_result = smoke_doom_impl(arch, false, true, false);
    let restore_result = build_impl(false, false, false, None, false);
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

fn smoke_proc_caps(arch_override: Option<String>) -> Result<()> {
    let arch = resolve_runtime_arch(arch_override)?;
    smoke_proc_caps_impl(arch)
}

fn smoke_proc_caps_impl(arch: RuntimeArch) -> Result<()> {
    ensure_runtime_artifacts(arch)?;

    let smoke_name = "smoke-proc-caps";
    let smoke_tag = format!("{smoke_name}-{}", arch.as_str());
    let time_denied_pattern = format!(
        "number={} ({}) denied",
        abi_shim::TIME_MS.number,
        abi_syscall::name(abi_shim::TIME_MS.number)
    );
    let socket_denied_pattern = format!(
        "number={} ({}) denied",
        abi_syscall::SYS_SOCKET,
        abi_syscall::name(abi_syscall::SYS_SOCKET)
    );
    let drop_core_denied_pattern = format!("drop_core_rc={}", abi_errno::EPERM);

    let mut qemu_cmd = Command::new("bash");
    qemu_cmd
        .args([arch.qemu_script()])
        .env("QEMU_DISPLAY", "none")
        .env("QEMU_AUDIO", "none");
    if arch == RuntimeArch::Aarch64 {
        qemu_cmd.env("QEMU_FB", "auto");
        qemu_cmd.env("QEMU_VIRTIO_BUS", "mmio");
    }
    qemu_cmd.env("QEMU_INPUT", "virtio");
    qemu_cmd.env(
        "QEMU_AUDIO_WAV_PATH",
        format!("target/{}/debug/{smoke_tag}.wav", arch.kernel_target()),
    );
    let mut child = qemu_cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to start qemu run for {smoke_tag}"))?;

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
        wait_for_log(
            &log,
            "arrost /home/user",
            Duration::from_secs(40),
            "shell prompt",
        )?;
        wait_for_log(
            &log,
            "[init] caps smoke: PASS",
            Duration::from_secs(8),
            "init capability smoke pass marker",
        )?;
        wait_for_log(
            &log,
            &time_denied_pattern,
            Duration::from_secs(8),
            "time capability denial",
        )?;
        wait_for_log(
            &log,
            &socket_denied_pattern,
            Duration::from_secs(8),
            "network capability denial",
        )?;
        wait_for_log(
            &log,
            &drop_core_denied_pattern,
            Duration::from_secs(8),
            "core capability drop denial",
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
        for pattern in ["ring3 exec:", "shell(exec):", "ls:", "ps: entries="] {
            if let Some(line) = last_matching_line(&log_snapshot, pattern) {
                eprintln!("debug[{pattern}]: {line}");
            }
        }
        eprintln!("----- serial tail -----");
        eprintln!("{}", log_tail(&log_snapshot, 80));
        return Err(error);
    }

    println!("{smoke_name}: PASS");
    if let Some(line) = last_matching_line(&log_snapshot, "[init] caps smoke: PASS") {
        println!("{smoke_name}: {line}");
    }
    if let Some(line) = last_matching_line(&log_snapshot, &socket_denied_pattern) {
        println!("{smoke_name}: {line}");
    }
    if let Some(line) = last_matching_line(&log_snapshot, &time_denied_pattern) {
        println!("{smoke_name}: {line}");
    }
    if let Some(line) = last_matching_line(&log_snapshot, &drop_core_denied_pattern) {
        println!("{smoke_name}: {line}");
    }
    Ok(())
}

fn smoke_proc_spawn(arch_override: Option<String>) -> Result<()> {
    let arch = resolve_runtime_arch(arch_override)?;
    smoke_proc_spawn_impl(arch)
}

fn smoke_proc_spawn_impl(arch: RuntimeArch) -> Result<()> {
    ensure_runtime_artifacts(arch)?;

    let smoke_name = "smoke-proc-spawn";
    let smoke_tag = format!("{smoke_name}-{}", arch.as_str());
    let init_exit_code = user_init::cooperative_exit_code();
    let doom_exit_code = user_doom::cooperative_exit_code();

    let mut qemu_cmd = Command::new("bash");
    qemu_cmd
        .args([arch.qemu_script()])
        .env("QEMU_DISPLAY", "none")
        .env("QEMU_AUDIO", "none");
    if arch == RuntimeArch::Aarch64 {
        qemu_cmd.env("QEMU_FB", "auto");
        qemu_cmd.env("QEMU_VIRTIO_BUS", "mmio");
    }
    qemu_cmd.env("QEMU_INPUT", "virtio");
    qemu_cmd.env(
        "QEMU_AUDIO_WAV_PATH",
        format!("target/{}/debug/{smoke_tag}.wav", arch.kernel_target()),
    );
    let mut child = qemu_cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to start qemu run for {smoke_tag}"))?;

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
        wait_for_log(
            &log,
            "arrost /home/user",
            Duration::from_secs(40),
            "shell prompt",
        )?;
        wait_for_log(
            &log,
            "[init] spawn/wait smoke: PASS",
            Duration::from_secs(12),
            "init spawn/wait smoke pass marker",
        )?;

        let stdin = child
            .stdin
            .as_mut()
            .context("failed to capture qemu stdin")?;
        send_serial_command(stdin, "user apps\n")?;
        let init_registry_line = format!(
            "user(app): id={} name={} caps={:#x} sleep={} exit={}",
            abi_syscall::app::INIT,
            user_init::app_name(),
            user_init::required_caps(),
            user_init::cooperative_sleep_ticks(),
            init_exit_code
        );
        wait_for_log(
            &log,
            &init_registry_line,
            Duration::from_secs(8),
            "init app registry contract line",
        )?;
        let doom_registry_line = format!(
            "user(app): id={} name={} caps={:#x} sleep={} exit={}",
            abi_syscall::app::DOOM,
            user_doom::app_name(),
            user_doom::required_caps(),
            user_doom::cooperative_sleep_ticks(),
            doom_exit_code
        );
        wait_for_log(
            &log,
            &doom_registry_line,
            Duration::from_secs(8),
            "doom app registry contract line",
        )?;

        send_serial_command(stdin, "spawn init\n")?;
        wait_for_log(
            &log,
            "user(spawn): app=init pid=",
            Duration::from_secs(8),
            "spawn command output",
        )?;
        wait_for_log(
            &log,
            "[uinit] init: ready (syscall ABI",
            Duration::from_secs(8),
            "init userland boot marker",
        )?;

        let spawn_snapshot = snapshot_log(&log);
        let Some(spawn_line) = last_matching_line(&spawn_snapshot, "user(spawn): app=init pid=")
        else {
            bail!("missing spawn confirmation line");
        };
        let Some(child_pid_u64) = parse_metric_value(spawn_line, "pid=") else {
            bail!("missing child pid in spawn confirmation");
        };
        let Ok(child_pid) = u32::try_from(child_pid_u64) else {
            bail!("invalid child pid in spawn confirmation: {child_pid_u64}");
        };

        send_serial_command(stdin, "wait all\n")?;
        wait_for_log(
            &log,
            "user(wait): all reaped=0 running=1",
            Duration::from_secs(8),
            "wait all running init confirmation",
        )?;

        send_serial_command(stdin, &format!("wait {child_pid}\n"))?;
        let wait_running = format!("user(wait): pid={child_pid} running");
        wait_for_log(
            &log,
            &wait_running,
            Duration::from_secs(8),
            "wait running confirmation",
        )?;

        thread::sleep(Duration::from_millis(1800));
        send_serial_command(stdin, "wait any\n")?;
        let wait_exit = format!("user(wait): any pid={child_pid} exit={init_exit_code}");
        wait_for_log(
            &log,
            &wait_exit,
            Duration::from_secs(8),
            "wait any init exit confirmation",
        )?;

        send_serial_command(stdin, "wait all\n")?;
        wait_for_log(
            &log,
            "user(wait): all reaped=0 running=0",
            Duration::from_secs(8),
            "wait all init drained confirmation",
        )?;

        send_serial_command(stdin, &format!("wait {child_pid}\n"))?;
        let wait_reaped = format!(
            "user(wait): failed pid={child_pid} rc={}",
            abi_errno::EINVAL
        );
        wait_for_log(
            &log,
            &wait_reaped,
            Duration::from_secs(8),
            "wait reaped confirmation",
        )?;

        send_serial_command(stdin, "spawn doom\n")?;
        wait_for_log(
            &log,
            "user(spawn): app=doom pid=",
            Duration::from_secs(8),
            "spawn doom command output",
        )?;
        wait_for_log(
            &log,
            "[udoom] doom: rust+c userland toolchain smoke ready",
            Duration::from_secs(8),
            "doom userland boot marker",
        )?;
        let spawn_snapshot = snapshot_log(&log);
        let Some(spawn_line) = last_matching_line(&spawn_snapshot, "user(spawn): app=doom pid=")
        else {
            bail!("missing doom spawn confirmation line");
        };
        let Some(doom_pid_u64) = parse_metric_value(spawn_line, "pid=") else {
            bail!("missing doom child pid in spawn confirmation");
        };
        let Ok(doom_pid) = u32::try_from(doom_pid_u64) else {
            bail!("invalid doom child pid in spawn confirmation: {doom_pid_u64}");
        };

        send_serial_command(stdin, "wait all\n")?;
        wait_for_log(
            &log,
            "user(wait): all reaped=0 running=1",
            Duration::from_secs(8),
            "wait all running doom confirmation",
        )?;

        send_serial_command(stdin, &format!("wait {doom_pid}\n"))?;
        let wait_running = format!("user(wait): pid={doom_pid} running");
        wait_for_log(
            &log,
            &wait_running,
            Duration::from_secs(8),
            "wait doom running confirmation",
        )?;

        thread::sleep(Duration::from_millis(2400));
        send_serial_command(stdin, "wait any\n")?;
        let wait_exit = format!("user(wait): any pid={doom_pid} exit={doom_exit_code}");
        wait_for_log(
            &log,
            &wait_exit,
            Duration::from_secs(8),
            "wait any doom exit confirmation",
        )?;

        send_serial_command(stdin, "wait all\n")?;
        wait_for_log(
            &log,
            "user(wait): all reaped=0 running=0",
            Duration::from_secs(8),
            "wait all doom drained confirmation",
        )?;

        send_serial_command(stdin, &format!("wait {doom_pid}\n"))?;
        let wait_reaped = format!("user(wait): failed pid={doom_pid} rc={}", abi_errno::EINVAL);
        wait_for_log(
            &log,
            &wait_reaped,
            Duration::from_secs(8),
            "wait doom reaped confirmation",
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
    if let Some(line) = last_matching_line(&log_snapshot, "[init] spawn/wait smoke: PASS") {
        println!("{smoke_name}: {line}");
    }
    if let Some(line) = last_matching_line(&log_snapshot, "user(app): id=") {
        println!("{smoke_name}: {line}");
    }
    if let Some(line) = last_matching_line(&log_snapshot, "user(spawn): app=init pid=") {
        println!("{smoke_name}: {line}");
    }
    if let Some(line) = last_matching_line(&log_snapshot, "user(spawn): app=doom pid=") {
        println!("{smoke_name}: {line}");
    }
    if let Some(line) = last_matching_line(&log_snapshot, "user(wait): pid=") {
        println!("{smoke_name}: {line}");
    }
    Ok(())
}

fn smoke_bin_exec(arch_override: Option<String>) -> Result<()> {
    let arch = resolve_runtime_arch(arch_override)?;
    let restore_force_fallback = env_truthy(DOOM_FORCE_FALLBACK_ENV);
    let restore_ring3_elf_groundwork = default_ring3_elf_groundwork_enabled();

    build_impl(restore_force_fallback, false, false, Some(true), false)?;
    let smoke_result = smoke_bin_exec_impl(arch);
    let restore_result = build_impl(
        restore_force_fallback,
        false,
        false,
        Some(restore_ring3_elf_groundwork),
        false,
    );
    match smoke_result {
        Ok(()) => {
            restore_result?;
            Ok(())
        }
        Err(smoke_err) => {
            if let Err(restore_err) = restore_result {
                return Err(smoke_err.context(format!(
                    "bin exec smoke failed and restoring prior ELF groundwork state failed: {restore_err:#}"
                )));
            }
            Err(smoke_err)
        }
    }
}

fn smoke_bin_exec_impl(arch: RuntimeArch) -> Result<()> {
    let _ = reset_storage_disk_image()?;
    ensure_runtime_artifacts(arch)?;

    let smoke_name = "smoke-bin-exec";
    let smoke_tag = format!("{smoke_name}-{}", arch.as_str());
    let mut qemu_cmd = Command::new("bash");
    qemu_cmd
        .args([arch.qemu_script()])
        .env("QEMU_DISPLAY", "none")
        .env("QEMU_AUDIO", "none");
    if arch == RuntimeArch::Aarch64 {
        qemu_cmd.env("QEMU_FB", "auto");
        qemu_cmd.env("QEMU_VIRTIO_BUS", "mmio");
    }
    qemu_cmd.env("QEMU_INPUT", "virtio");
    qemu_cmd.env(
        "QEMU_AUDIO_WAV_PATH",
        format!("target/{}/debug/{smoke_tag}.wav", arch.kernel_target()),
    );
    let mut child = qemu_cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to start qemu run for {smoke_tag}"))?;

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
        wait_for_log(
            &log,
            "arrost /home/user",
            Duration::from_secs(40),
            "shell prompt",
        )?;
        wait_for_log(
            &log,
            "mount: / type=",
            Duration::from_secs(8),
            "root mount table log",
        )?;
        wait_for_log(
            &log,
            "mount: /proc type=procfs ro",
            Duration::from_secs(8),
            "procfs mount table log",
        )?;
        wait_for_log(
            &log,
            "mount: /tmp type=tmpfs rw",
            Duration::from_secs(8),
            "tmpfs mount table log",
        )?;
        wait_for_log(
            &log,
            "[init] spawn/wait smoke: PASS",
            Duration::from_secs(8),
            "init spawn/wait smoke completion",
        )?;
        wait_for_log(
            &log,
            "[init] exit(0)",
            Duration::from_secs(8),
            "init task exit",
        )?;
        let stdin = child
            .stdin
            .as_mut()
            .context("failed to capture qemu stdin")?;

        send_serial_command(stdin, "ls /bin\n")?;
        thread::sleep(Duration::from_millis(250));
        let bin_snapshot = snapshot_log(&log);
        let bin_lines = lines_after_matching_until_prompt(&bin_snapshot, "ls /bin");
        for marker in [
            "ls", "ps", "kill", "cat", "echo", "fm", "doom", "terminal", "link", "symlink",
        ] {
            if !bin_lines.contains(&marker) {
                bail!("missing `{marker}` in ls /bin output: {bin_lines:?}");
            }
        }

        send_serial_command(stdin, "ps\n")?;
        wait_for_log(
            &log,
            "name=/bin/ps",
            Duration::from_secs(8),
            "binary ps process table line",
        )?;
        let ps_snapshot = snapshot_log(&log);
        let Some(ps_line) = last_matching_line(&ps_snapshot, "name=/bin/ps") else {
            bail!("missing /bin/ps process entry in process table output");
        };
        if !ps_line.contains("kind=binary") {
            bail!("expected /bin/ps entry with kind=binary");
        }
        let Some(ps_pid) = parse_metric_value(ps_line, "pid=") else {
            bail!("missing pid in /bin/ps process entry");
        };
        if ps_pid == 0 {
            bail!("invalid /bin/ps pid in process entry");
        }

        send_serial_command(stdin, "/bin/cat README.TXT\n")?;
        wait_for_log(
            &log,
            "cat: ",
            Duration::from_secs(8),
            "bin cat output header",
        )?;
        wait_for_log(
            &log,
            "README.TXT",
            Duration::from_secs(8),
            "bin cat target file marker",
        )?;

        send_serial_command(stdin, "ls /proc\n")?;
        thread::sleep(Duration::from_millis(250));
        let proc_snapshot = snapshot_log(&log);
        let proc_lines = lines_after_matching_until_prompt(&proc_snapshot, "ls /proc");
        for marker in ["self", "mounts", "uptime"] {
            if !proc_lines.contains(&marker) {
                bail!("missing `{marker}` in ls /proc output: {proc_lines:?}");
            }
        }

        send_serial_command(stdin, "cat /proc/self/pid\n")?;
        wait_for_log(
            &log,
            "cat: ",
            Duration::from_secs(8),
            "proc self pid cat header",
        )?;
        wait_for_log(
            &log,
            "/proc/self/pid",
            Duration::from_secs(8),
            "proc self pid target marker",
        )?;
        let proc_pid_snapshot = snapshot_log(&log);
        let Some(pid_line) = line_after_matching(&proc_pid_snapshot, "/proc/self/pid") else {
            bail!("missing pid line after /proc/self/pid output");
        };
        let proc_pid = pid_line
            .trim()
            .parse::<u64>()
            .with_context(|| format!("failed to parse /proc/self/pid output `{pid_line}`"))?;
        if proc_pid == 0 {
            bail!("expected /proc/self/pid to return a non-zero pid");
        }

        send_serial_command(stdin, "ls /tmp\n")?;
        wait_for_log(
            &log,
            "path=/tmp",
            Duration::from_secs(8),
            "tmpfs listing header",
        )?;

        send_serial_command(stdin, "echo tmp-smoke > /tmp/TMPSMOKE.TXT\n")?;
        wait_for_log(
            &log,
            "echo: wrote ",
            Duration::from_secs(8),
            "tmpfs echo write output",
        )?;
        wait_for_log(
            &log,
            "/tmp/TMPSMOKE.TXT",
            Duration::from_secs(8),
            "tmpfs echo target marker",
        )?;

        send_serial_command(stdin, "cat /tmp/TMPSMOKE.TXT\n")?;
        wait_for_log(
            &log,
            "/tmp/TMPSMOKE.TXT",
            Duration::from_secs(8),
            "tmpfs cat target marker",
        )?;
        wait_for_log(
            &log,
            "tmp-smoke",
            Duration::from_secs(8),
            "tmpfs cat payload",
        )?;

        send_serial_command(stdin, "ls /tmp\n")?;
        wait_for_log(
            &log,
            "TMPSMOKE.TXT",
            Duration::from_secs(8),
            "tmpfs listing entry",
        )?;

        send_serial_command(stdin, "cat /TMPSMOKE.TXT\n")?;
        wait_for_log(
            &log,
            "cat: /TMPSMOKE.TXT (not_found)",
            Duration::from_secs(8),
            "tmpfs isolation check",
        )?;

        send_serial_command(stdin, "/bin/echo bin-smoke > BINSMOKE.TXT\n")?;
        wait_for_log(
            &log,
            "echo: wrote ",
            Duration::from_secs(8),
            "bin echo write output",
        )?;

        send_serial_command(stdin, "/bin/fm open BINSMOKE.TXT\n")?;
        wait_for_log(&log, "cat: ", Duration::from_secs(8), "bin fm open output")?;
        wait_for_log(
            &log,
            "BINSMOKE.TXT",
            Duration::from_secs(8),
            "bin fm open target marker",
        )?;

        send_serial_command(stdin, "echo hard-smoke > LINKSRC.TXT\n")?;
        wait_for_log(
            &log,
            "LINKSRC.TXT",
            Duration::from_secs(8),
            "hard-link source creation",
        )?;

        send_serial_command(stdin, "link LINKSRC.TXT LINKDST.TXT\n")?;
        wait_for_log(
            &log,
            "link: /LINKSRC.TXT -> /LINKDST.TXT",
            Duration::from_secs(8),
            "hard-link creation output",
        )?;

        send_serial_command(stdin, "fm delete LINKSRC.TXT\n")?;
        wait_for_log(
            &log,
            "fm: deleted /LINKSRC.TXT",
            Duration::from_secs(8),
            "hard-link source deletion",
        )?;

        send_serial_command(stdin, "cat LINKDST.TXT\n")?;
        wait_for_log(
            &log,
            "LINKDST.TXT",
            Duration::from_secs(8),
            "hard-link cat target marker",
        )?;
        wait_for_log(
            &log,
            "hard-smoke",
            Duration::from_secs(8),
            "hard-link preserved payload",
        )?;

        send_serial_command(stdin, "symlink LINKDST.TXT LINKSYM.TXT\n")?;
        wait_for_log(
            &log,
            "symlink: /LINKSYM.TXT -> LINKDST.TXT",
            Duration::from_secs(8),
            "symlink creation output",
        )?;

        send_serial_command(stdin, "cat LINKSYM.TXT\n")?;
        wait_for_log(
            &log,
            "LINKSYM.TXT",
            Duration::from_secs(8),
            "symlink cat target marker",
        )?;
        wait_for_log(
            &log,
            "hard-smoke",
            Duration::from_secs(8),
            "symlink followed payload",
        )?;

        send_serial_command(stdin, "symlink LOOP_B LOOP_A\n")?;
        wait_for_log(
            &log,
            "symlink: /LOOP_A -> LOOP_B",
            Duration::from_secs(8),
            "loop symlink A creation",
        )?;

        send_serial_command(stdin, "symlink LOOP_A LOOP_B\n")?;
        wait_for_log(
            &log,
            "symlink: /LOOP_B -> LOOP_A",
            Duration::from_secs(8),
            "loop symlink B creation",
        )?;

        send_serial_command(stdin, "cat LOOP_A\n")?;
        wait_for_log(
            &log,
            "cat: /LOOP_A (eloop)",
            Duration::from_secs(8),
            "symlink loop detection",
        )?;

        send_serial_command(stdin, "/bin/doom status\n")?;
        wait_for_log(
            &log,
            "doom: app=",
            Duration::from_secs(8),
            "bin doom status output",
        )?;
        send_serial_command(stdin, "waitx all\n")?;
        wait_for_log(
            &log,
            "external(wait): all reaped=",
            Duration::from_secs(8),
            "external waitx all output after bin wrappers",
        )?;
        let waitx_snapshot = snapshot_log(&log);
        let Some(waitx_line) = last_matching_line(&waitx_snapshot, "external(wait): all reaped=")
        else {
            bail!("missing waitx all output after /bin wrappers");
        };
        let Some(waitx_reaped) = parse_metric_value(waitx_line, "reaped=") else {
            bail!("missing reaped metric in waitx all output");
        };
        if waitx_reaped == 0 {
            bail!("expected waitx all to reap at least one external /bin wrapper process");
        }

        send_serial_shell_command(stdin, "doom play\n")?;
        if wait_for_log(
            &log,
            "doom: play mode started (doomgeneric)",
            Duration::from_secs(8),
            "doom play confirmation",
        )
        .is_err()
        {
            wait_for_log(
                &log,
                "doom: doomgeneric not ready; starting fallback runtime",
                Duration::from_secs(8),
                "doom fallback play confirmation",
            )?;
        }
        send_serial_shell_command(stdin, "ps\n")?;
        wait_for_log(
            &log,
            "name=doom",
            Duration::from_secs(8),
            "doom runtime process table line",
        )?;
        let doom_snapshot = snapshot_log(&log);
        let Some(doom_line) = last_matching_line(&doom_snapshot, "name=doom") else {
            bail!("missing doom runtime process entry in process table output");
        };
        if !doom_line.contains("kind=doom-runtime") {
            bail!("expected doom runtime process entry with kind=doom-runtime");
        }
        let Some(doom_pid) = parse_metric_value(doom_line, "pid=") else {
            bail!("missing pid in doom runtime process entry");
        };
        if doom_pid == 0 {
            bail!("invalid doom runtime pid in process entry");
        }
        wait_for_log(
            &log,
            "arrost /home/user",
            Duration::from_secs(6),
            "shell prompt before doom kill",
        )?;
        send_serial_shell_command(stdin, &format!("kill {doom_pid}\n"))?;
        wait_for_log(
            &log,
            &format!("kill: pid={} rc=0", doom_pid),
            Duration::from_secs(16),
            "doom kill output",
        )?;
        send_serial_shell_command(stdin, &format!("waitx {doom_pid}\n"))?;
        wait_for_log(
            &log,
            &format!("external(wait): pid={} exit=", doom_pid),
            Duration::from_secs(16),
            "doom waitx reap output",
        )?;

        send_serial_command(stdin, "terminal\n")?;
        wait_for_log(
            &log,
            "terminal: launched",
            Duration::from_secs(8),
            "shell terminal launch output",
        )?;
        send_serial_command(stdin, "ps\n")?;
        wait_for_log(
            &log,
            "name=terminal",
            Duration::from_secs(8),
            "external terminal process table line",
        )?;
        let terminal_snapshot = snapshot_log(&log);
        let Some(terminal_line) = last_matching_line(&terminal_snapshot, "name=terminal") else {
            bail!("missing terminal process entry in process table output");
        };
        let Some(terminal_pid) = parse_metric_value(terminal_line, "pid=") else {
            bail!("missing pid in terminal process entry");
        };
        if terminal_pid == 0 {
            bail!("invalid terminal pid in process entry");
        }

        send_serial_command(stdin, &format!("kill {terminal_pid}\n"))?;
        wait_for_log(
            &log,
            &format!("kill: pid={} rc=0", terminal_pid),
            Duration::from_secs(8),
            "terminal kill output",
        )?;
        send_serial_command(stdin, &format!("waitx {terminal_pid}\n"))?;
        wait_for_log(
            &log,
            &format!("external(wait): pid={} exit=", terminal_pid),
            Duration::from_secs(8),
            "external waitx terminal reap output",
        )?;

        send_serial_command(stdin, "/bin/kill self\n")?;
        wait_for_log(
            &log,
            "kill: pid=",
            Duration::from_secs(8),
            "bin kill self output",
        )?;
        let kill_snapshot = snapshot_log(&log);
        let Some(kill_line) = last_matching_line(&kill_snapshot, "kill: pid=") else {
            bail!("missing kill result line for /bin/kill self");
        };
        let Some(kill_pid) = parse_metric_value(kill_line, "pid=") else {
            bail!("missing pid in /bin/kill self result");
        };
        let Some(kill_rc) = parse_metric_value(kill_line, "rc=") else {
            bail!("missing rc in /bin/kill self result");
        };
        if kill_pid == 0 {
            bail!("invalid pid in /bin/kill self result");
        }
        if kill_rc != 0 {
            bail!("expected /bin/kill self to succeed (rc={kill_rc})");
        }

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
    if let Some(line) = last_matching_line(&log_snapshot, "mount: /proc type=procfs ro") {
        println!("{smoke_name}: {line}");
    }
    if let Some(line) = last_matching_line(&log_snapshot, "name=/bin/ps") {
        println!("{smoke_name}: {line}");
    }
    if let Some(line) = last_matching_line(&log_snapshot, "path=/tmp") {
        println!("{smoke_name}: {line}");
    }
    if let Some(line) = last_matching_line(&log_snapshot, "kill: pid=") {
        println!("{smoke_name}: {line}");
    }
    if let Some(line) = last_matching_line(&log_snapshot, "BINSMOKE.TXT") {
        println!("{smoke_name}: {line}");
    }
    Ok(())
}

fn smoke_net(arch_override: Option<String>) -> Result<()> {
    let arch = resolve_runtime_arch(arch_override)?;
    smoke_net_impl(arch)
}

fn smoke_net_impl(arch: RuntimeArch) -> Result<()> {
    ensure_runtime_artifacts(arch)?;

    let smoke_name = "smoke-net";
    let smoke_tag = format!("{smoke_name}-{}", arch.as_str());
    let mut qemu_cmd = Command::new("bash");
    qemu_cmd
        .args([arch.qemu_script()])
        .env("QEMU_DISPLAY", "none")
        .env("QEMU_AUDIO", "none");
    if arch == RuntimeArch::Aarch64 {
        qemu_cmd.env("QEMU_FB", "auto");
        qemu_cmd.env("QEMU_VIRTIO_BUS", "mmio");
    }
    qemu_cmd.env("QEMU_INPUT", "virtio");
    qemu_cmd.env(
        "QEMU_AUDIO_WAV_PATH",
        format!("target/{}/debug/{smoke_tag}.wav", arch.kernel_target()),
    );
    let mut child = qemu_cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to start qemu run for {smoke_tag}"))?;

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
        wait_for_log(
            &log,
            "arrost /home/user",
            Duration::from_secs(40),
            "shell prompt",
        )?;
        let stdin = child
            .stdin
            .as_mut()
            .context("failed to capture qemu stdin")?;

        // Verify net info is available
        send_serial_command(stdin, "net\n")?;
        wait_for_log(&log, "net: backend=", Duration::from_secs(8), "net info")?;

        // netstat shows connection table header
        send_serial_command(stdin, "netstat\n")?;
        wait_for_log(
            &log,
            "netstat: Active Internet connections",
            Duration::from_secs(8),
            "netstat header",
        )?;

        // ifconfig shows eth0 line
        send_serial_command(stdin, "ifconfig\n")?;
        wait_for_log(
            &log,
            "ifconfig: eth0:",
            Duration::from_secs(8),
            "ifconfig eth0 line",
        )?;

        // route shows routing table header
        send_serial_command(stdin, "route\n")?;
        wait_for_log(
            &log,
            "route: Kernel IP routing table",
            Duration::from_secs(8),
            "route table header",
        )?;

        // arp shows ARP cache header
        send_serial_command(stdin, "arp\n")?;
        wait_for_log(
            &log,
            "arp: Address",
            Duration::from_secs(8),
            "arp cache header",
        )?;

        // ss shows socket summary header
        send_serial_command(stdin, "ss\n")?;
        wait_for_log(
            &log,
            "ss: Netid",
            Duration::from_secs(8),
            "ss socket header",
        )?;

        // ip addr shows interface info
        send_serial_command(stdin, "ip addr\n")?;
        wait_for_log(
            &log,
            "ifconfig: eth0:",
            Duration::from_secs(8),
            "ip addr output (delegates to ifconfig)",
        )?;

        // Verify /bin/netstat is in ls /bin output
        send_serial_command(stdin, "ls /bin\n")?;
        thread::sleep(Duration::from_millis(250));
        let ls_snapshot = snapshot_log(&log);
        let ls_lines = lines_after_matching_until_prompt(&ls_snapshot, "ls /bin");
        for marker in ["netstat", "ifconfig", "ip"] {
            if !ls_lines.contains(&marker) {
                bail!("missing `{marker}` in ls /bin output: {ls_lines:?}");
            }
        }

        Ok(())
    })();

    let kill_result = child.kill();
    let _ = stdout_reader.join();
    let _ = stderr_reader.join();

    let log_snapshot = snapshot_log(&log);
    if smoke_result.is_err() {
        eprintln!("--- {smoke_name} log ---");
        eprintln!("{log_snapshot}");
        eprintln!("--- end {smoke_name} log ---");
    }
    kill_result.with_context(|| format!("failed to kill qemu for {smoke_tag}"))?;
    smoke_result?;
    if let Some(line) = last_matching_line(&log_snapshot, "net: backend=") {
        println!("{smoke_name}: {line}");
    }
    if let Some(line) = last_matching_line(&log_snapshot, "netstat: Active") {
        println!("{smoke_name}: {line}");
    }
    Ok(())
}

fn smoke_fs(arch_override: Option<String>) -> Result<()> {
    let arch = resolve_runtime_arch(arch_override)?;
    let restore_force_fallback = env_truthy(DOOM_FORCE_FALLBACK_ENV);
    let restore_ring3_elf_groundwork = default_ring3_elf_groundwork_enabled();

    build_impl(restore_force_fallback, false, false, Some(true), true)?;
    let smoke_result = smoke_fs_impl(arch);
    let restore_result = build_impl(
        restore_force_fallback,
        false,
        false,
        Some(restore_ring3_elf_groundwork),
        false,
    );
    match smoke_result {
        Ok(()) => {
            restore_result?;
            Ok(())
        }
        Err(smoke_err) => {
            if let Err(restore_err) = restore_result {
                return Err(smoke_err.context(format!(
                    "fs smoke failed and restoring prior ELF groundwork state failed: {restore_err:#}"
                )));
            }
            Err(smoke_err)
        }
    }
}

fn smoke_fs_impl(arch: RuntimeArch) -> Result<()> {
    let _ = reset_storage_disk_image()?;
    ensure_runtime_artifacts(arch)?;

    let smoke_name = "smoke-fs";
    let smoke_tag = format!("{smoke_name}-{}", arch.as_str());
    let mut qemu_cmd = Command::new("bash");
    qemu_cmd
        .args([arch.qemu_script()])
        .env("QEMU_DISPLAY", "none")
        .env("QEMU_AUDIO", "none");
    if arch == RuntimeArch::Aarch64 {
        qemu_cmd.env("QEMU_FB", "auto");
        qemu_cmd.env("QEMU_VIRTIO_BUS", "mmio");
    }
    qemu_cmd.env("QEMU_INPUT", "virtio");
    qemu_cmd.env(
        "QEMU_AUDIO_WAV_PATH",
        format!("target/{}/debug/{smoke_tag}.wav", arch.kernel_target()),
    );
    let mut child = qemu_cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to start qemu run for {smoke_tag}"))?;

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
        wait_for_log(
            &log,
            "arrost /home/user",
            Duration::from_secs(40),
            "shell prompt",
        )?;
        let stdin = child
            .stdin
            .as_mut()
            .context("failed to capture qemu stdin")?;

        send_serial_command(stdin, "pwd\n")?;
        wait_for_line_after(
            &log,
            "pwd",
            "/home/user",
            Duration::from_secs(8),
            "initial pwd output",
        )?;

        send_serial_command(stdin, "ls -a\n")?;
        thread::sleep(Duration::from_millis(250));
        let home_snapshot = snapshot_log(&log);
        let home_lines = lines_after_matching_until_prompt(&home_snapshot, "ls -a");
        if !home_lines.contains(&".history") {
            bail!("expected .history in ls -a output, got {home_lines:?}");
        }

        send_serial_command(stdin, "ls -lsa\n")?;
        thread::sleep(Duration::from_millis(250));
        let long_snapshot = snapshot_log(&log);
        let long_lines = lines_after_matching_until_prompt(&long_snapshot, "ls -lsa");
        if !long_lines.iter().any(|line| line.starts_with("total ")) {
            bail!("expected total line in ls -lsa output, got {long_lines:?}");
        }
        if !long_lines.iter().any(|line| line.contains(".history")) {
            bail!("expected .history in ls -lsa output, got {long_lines:?}");
        }

        send_serial_command(stdin, "cd /bin\n")?;
        wait_for_log(&log, "cd /bin", Duration::from_secs(8), "cd /bin echo")?;
        send_serial_command(stdin, "pwd\n")?;
        wait_for_line_after(
            &log,
            "pwd",
            "/bin",
            Duration::from_secs(8),
            "pwd after cd /bin",
        )?;

        send_serial_command(stdin, "sy\t\n")?;
        wait_for_log(
            &log,
            "usage: symlink <target> <linkpath>",
            Duration::from_secs(8),
            "tab completion symlink usage",
        )?;

        send_serial_command(stdin, "ls -ls\n")?;
        thread::sleep(Duration::from_millis(250));
        let ls_bin_snapshot = snapshot_log(&log);
        let ls_bin_lines = lines_after_matching_until_prompt(&ls_bin_snapshot, "ls -ls");
        if !ls_bin_lines.iter().any(|line| line.starts_with("total ")) {
            bail!("expected total line in ls -ls output, got {ls_bin_lines:?}");
        }
        if !ls_bin_lines.iter().any(|line| line.contains("ls")) {
            bail!("expected ls entry in ls -ls output, got {ls_bin_lines:?}");
        }

        send_serial_command(stdin, "cd ..\n")?;
        wait_for_log(&log, "cd ..", Duration::from_secs(8), "cd .. echo")?;
        send_serial_command(stdin, "pwd\n")?;
        wait_for_line_after(&log, "pwd", "/", Duration::from_secs(8), "pwd after cd ..")?;

        send_serial_command(stdin, "cd\n")?;
        send_serial_command(stdin, "pwd\n")?;
        wait_for_line_after(
            &log,
            "pwd",
            "/home/user",
            Duration::from_secs(8),
            "pwd after cd home",
        )?;

        send_serial_command(stdin, "echo test > file.txt\n")?;
        wait_for_log(
            &log,
            "file.txt",
            Duration::from_secs(8),
            "echo file.txt output",
        )?;
        send_serial_command(stdin, "cat file.txt\n")?;
        wait_for_log(
            &log,
            "file.txt",
            Duration::from_secs(8),
            "cat file.txt header",
        )?;
        wait_for_log(&log, "test", Duration::from_secs(8), "cat file.txt payload")?;

        send_serial_command(stdin, "echo tab-smoke > lol.txt\n")?;
        wait_for_log(
            &log,
            "lol.txt",
            Duration::from_secs(8),
            "echo lol.txt output",
        )?;
        send_serial_command(stdin, "cat lo\t\n")?;
        wait_for_log(
            &log,
            "cat: /home/user/lol.txt",
            Duration::from_secs(8),
            "path completion cat header",
        )?;
        wait_for_log(
            &log,
            "tab-smoke",
            Duration::from_secs(8),
            "path completion cat payload",
        )?;

        send_serial_command(stdin, "cat /home/user/.history\n")?;
        wait_for_log(
            &log,
            "cat: /home/user/.history",
            Duration::from_secs(8),
            "history cat header",
        )?;
        thread::sleep(Duration::from_millis(250));
        let history_snapshot = snapshot_log(&log);
        let Some((_, history_output)) = history_snapshot.rsplit_once("cat: /home/user/.history")
        else {
            bail!("missing cat /home/user/.history output in log");
        };
        for marker in [
            "pwd",
            "ls -a",
            "ls -lsa",
            "cd /bin",
            "symlink",
            "echo test > file.txt",
            "cat lol.txt",
        ] {
            if !history_output.lines().any(|line| line.trim() == marker) {
                bail!("expected `{marker}` in history output: {history_output:?}");
            }
        }

        send_serial_command(stdin, "echo cache-smoke > /DCACHE.TXT\n")?;
        wait_for_log(
            &log,
            "/DCACHE.TXT",
            Duration::from_secs(8),
            "DCACHE create output",
        )?;
        send_serial_command(stdin, "cat /DCACHE.TXT\n")?;
        wait_for_log(
            &log,
            "cat: /DCACHE.TXT",
            Duration::from_secs(8),
            "DCACHE first cat header",
        )?;
        send_serial_command(stdin, "cat /DCACHE.TXT\n")?;
        wait_for_log(
            &log,
            "dentry: hit path=/DCACHE.TXT",
            Duration::from_secs(8),
            "DCACHE cache hit",
        )?;

        send_serial_command(stdin, "echo rename-cache > /DENTRYOLD.TXT\n")?;
        wait_for_log(
            &log,
            "/DENTRYOLD.TXT",
            Duration::from_secs(8),
            "DENTRYOLD create output",
        )?;
        send_serial_command(stdin, "cat /DENTRYOLD.TXT\n")?;
        wait_for_log(
            &log,
            "cat: /DENTRYOLD.TXT",
            Duration::from_secs(8),
            "DENTRYOLD first cat header",
        )?;
        send_serial_command(stdin, "cat /DENTRYOLD.TXT\n")?;
        wait_for_log(
            &log,
            "dentry: hit path=/DENTRYOLD.TXT",
            Duration::from_secs(8),
            "DENTRYOLD cache hit",
        )?;
        send_serial_command(stdin, "mv /DENTRYOLD.TXT /DENTRYNEW.TXT\n")?;
        wait_for_log(
            &log,
            "mv: /DENTRYOLD.TXT -> /DENTRYNEW.TXT",
            Duration::from_secs(8),
            "DENTRY rename output",
        )?;
        wait_for_log(
            &log,
            "dentry: invalidate reason=rename",
            Duration::from_secs(8),
            "DENTRY rename invalidation",
        )?;
        send_serial_command(stdin, "cat /DENTRYOLD.TXT\n")?;
        wait_for_log(
            &log,
            "cat: /DENTRYOLD.TXT (not_found)",
            Duration::from_secs(8),
            "DENTRYOLD stale path miss",
        )?;
        send_serial_command(stdin, "cat /DENTRYNEW.TXT\n")?;
        wait_for_log(
            &log,
            "cat: /DENTRYNEW.TXT",
            Duration::from_secs(8),
            "DENTRYNEW cat header",
        )?;

        send_serial_command(stdin, "echo unlink-cache > /DENTRYUNLINK.TXT\n")?;
        wait_for_log(
            &log,
            "/DENTRYUNLINK.TXT",
            Duration::from_secs(8),
            "DENTRYUNLINK create output",
        )?;
        send_serial_command(stdin, "cat /DENTRYUNLINK.TXT\n")?;
        wait_for_log(
            &log,
            "cat: /DENTRYUNLINK.TXT",
            Duration::from_secs(8),
            "DENTRYUNLINK first cat header",
        )?;
        send_serial_command(stdin, "cat /DENTRYUNLINK.TXT\n")?;
        wait_for_log(
            &log,
            "dentry: hit path=/DENTRYUNLINK.TXT",
            Duration::from_secs(8),
            "DENTRYUNLINK cache hit",
        )?;
        send_serial_command(stdin, "fm delete /DENTRYUNLINK.TXT\n")?;
        wait_for_log(
            &log,
            "fm: deleted /DENTRYUNLINK.TXT",
            Duration::from_secs(8),
            "DENTRYUNLINK delete output",
        )?;
        wait_for_log(
            &log,
            "dentry: invalidate reason=unlink",
            Duration::from_secs(8),
            "DENTRYUNLINK invalidation",
        )?;
        send_serial_command(stdin, "cat /DENTRYUNLINK.TXT\n")?;
        wait_for_log(
            &log,
            "cat: /DENTRYUNLINK.TXT (not_found)",
            Duration::from_secs(8),
            "DENTRYUNLINK stale path miss",
        )?;

        send_serial_command(stdin, "echo move-smoke > /old.txt\n")?;
        wait_for_log(
            &log,
            "/old.txt",
            Duration::from_secs(8),
            "old file create output",
        )?;
        send_serial_command(stdin, "mv /old.txt /new.txt\n")?;
        wait_for_log(
            &log,
            "mv: /old.txt -> /new.txt",
            Duration::from_secs(8),
            "mv output",
        )?;
        send_serial_command(stdin, "cat /new.txt\n")?;
        wait_for_log(
            &log,
            "/new.txt",
            Duration::from_secs(8),
            "cat /new.txt header",
        )?;
        wait_for_log(
            &log,
            "move-smoke",
            Duration::from_secs(8),
            "cat /new.txt payload",
        )?;

        send_serial_command(stdin, "stat /README.TXT\n")?;
        wait_for_log(
            &log,
            "stat: path=/README.TXT",
            Duration::from_secs(8),
            "README stat output",
        )?;
        let readme_stat_snapshot = snapshot_log(&log);
        let Some(readme_stat_line) =
            last_matching_line(&readme_stat_snapshot, "stat: path=/README.TXT")
        else {
            bail!("missing README stat line");
        };
        if !readme_stat_line.contains("uid=0") || !readme_stat_line.contains("gid=0") {
            bail!("expected README stat to report uid=0 gid=0");
        }

        send_serial_command(stdin, "echo stamp-one > /STAMP.TXT\n")?;
        wait_for_log(
            &log,
            "/STAMP.TXT",
            Duration::from_secs(8),
            "STAMP create output",
        )?;
        send_serial_command(stdin, "stat /STAMP.TXT\n")?;
        wait_for_log(
            &log,
            "stat: path=/STAMP.TXT",
            Duration::from_secs(8),
            "initial STAMP stat",
        )?;
        let stamp_stat_snapshot = snapshot_log(&log);
        let Some(stamp_stat_line_1) =
            last_matching_line(&stamp_stat_snapshot, "stat: path=/STAMP.TXT")
        else {
            bail!("missing initial STAMP stat line");
        };
        let Some(stamp_mtime_1) = parse_metric_value(stamp_stat_line_1, "mtime=") else {
            bail!("missing mtime in initial STAMP stat");
        };
        let Some(stamp_atime_1) = parse_metric_value(stamp_stat_line_1, "atime=") else {
            bail!("missing atime in initial STAMP stat");
        };

        thread::sleep(Duration::from_millis(60));
        send_serial_command(stdin, "echo stamp-two > /STAMP.TXT\n")?;
        wait_for_log(
            &log,
            "/STAMP.TXT",
            Duration::from_secs(8),
            "STAMP overwrite output",
        )?;
        send_serial_command(stdin, "stat /STAMP.TXT\n")?;
        let stamp_stat_line_2 = wait_for_new_matching_line(
            &log,
            "stat: path=/STAMP.TXT",
            stamp_stat_line_1,
            Duration::from_secs(8),
            "second STAMP stat",
        )?;
        let Some(stamp_mtime_2) = parse_metric_value(&stamp_stat_line_2, "mtime=") else {
            bail!("missing mtime in second STAMP stat");
        };
        let Some(stamp_atime_2) = parse_metric_value(&stamp_stat_line_2, "atime=") else {
            bail!("missing atime in second STAMP stat");
        };
        if stamp_mtime_2 <= stamp_mtime_1 {
            bail!(
                "expected STAMP mtime to increase after write (before={} after={})",
                stamp_mtime_1,
                stamp_mtime_2
            );
        }
        if stamp_atime_2 < stamp_atime_1 {
            bail!("expected STAMP atime to stay monotonic across stats");
        }

        thread::sleep(Duration::from_millis(60));
        send_serial_command(stdin, "cat /STAMP.TXT\n")?;
        wait_for_log(
            &log,
            "stamp-two",
            Duration::from_secs(8),
            "STAMP cat payload",
        )?;
        send_serial_command(stdin, "stat /STAMP.TXT\n")?;
        let stamp_stat_line_3 = wait_for_new_matching_line(
            &log,
            "stat: path=/STAMP.TXT",
            &stamp_stat_line_2,
            Duration::from_secs(8),
            "third STAMP stat",
        )?;
        let Some(stamp_atime_3) = parse_metric_value(&stamp_stat_line_3, "atime=") else {
            bail!("missing atime in third STAMP stat");
        };
        if stamp_atime_3 <= stamp_atime_2 {
            bail!(
                "expected STAMP atime to increase after read (before={} after={})",
                stamp_atime_2,
                stamp_atime_3
            );
        }

        send_serial_command(stdin, "chmod 700 /README.TXT\n")?;
        wait_for_log(
            &log,
            "chmod: /README.TXT mode=0o700",
            Duration::from_secs(8),
            "chmod README output",
        )?;
        send_serial_command(stdin, "stat /README.TXT\n")?;
        wait_for_log(
            &log,
            "mode=0o100700",
            Duration::from_secs(8),
            "README chmod stat mode",
        )?;

        send_serial_command(stdin, "ring3 groundwork\n")?;
        wait_for_log(
            &log,
            "ring3(groundwork):",
            Duration::from_secs(12),
            "ring3 groundwork output",
        )?;
        let groundwork_snapshot = snapshot_log(&log);
        let Some(groundwork_line) = last_matching_line(&groundwork_snapshot, "ring3(groundwork):")
        else {
            bail!("missing ring3 groundwork line");
        };
        let Some(fd_open_readme_rc) = parse_signed_metric_value(groundwork_line, "fd_open_readme=")
        else {
            bail!("missing fd_open_readme metric in ring3 groundwork output");
        };
        if fd_open_readme_rc < 0 {
            bail!(
                "expected ring3 README open to succeed with read permissions, got {fd_open_readme_rc}"
            );
        }
        let Some(fd_open_tmp_rc) = parse_signed_metric_value(groundwork_line, "fd_open_tmp=")
        else {
            bail!("missing fd_open_tmp metric in ring3 groundwork output");
        };
        if fd_open_tmp_rc < 0 {
            bail!("expected ring3 tmp open/create to succeed, got {fd_open_tmp_rc}");
        }

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
    if let Some(line) = last_matching_line(&log_snapshot, "stat: path=/README.TXT") {
        println!("{smoke_name}: {line}");
    }
    if let Some(line) = last_matching_line(&log_snapshot, "stat: path=/STAMP.TXT") {
        println!("{smoke_name}: {line}");
    }
    if let Some(line) = last_matching_line(&log_snapshot, "dentry: hit path=/DCACHE.TXT") {
        println!("{smoke_name}: {line}");
    }
    if let Some(line) = last_matching_line(&log_snapshot, "dentry: invalidate reason=rename") {
        println!("{smoke_name}: {line}");
    }
    if let Some(line) = last_matching_line(&log_snapshot, "dentry: invalidate reason=unlink") {
        println!("{smoke_name}: {line}");
    }
    if let Some(line) = last_matching_line(&log_snapshot, "ring3(groundwork):") {
        println!("{smoke_name}: {line}");
    }
    Ok(())
}

fn smoke_ring3(arch_override: Option<String>) -> Result<()> {
    let arch = resolve_runtime_arch(arch_override)?;
    smoke_ring3_impl(arch)
}

fn smoke_ring3_impl(arch: RuntimeArch) -> Result<()> {
    ensure_runtime_artifacts(arch)?;

    let smoke_name = "smoke-ring3";
    let smoke_tag = format!("{smoke_name}-{}", arch.as_str());
    let expected_transition = match arch {
        RuntimeArch::X86_64 => "hw_transition=x86_64-int80",
        RuntimeArch::Aarch64 => "hw_transition=aarch64-svc",
    };
    let mut qemu_cmd = Command::new("bash");
    qemu_cmd
        .args([arch.qemu_script()])
        .env("QEMU_DISPLAY", "none")
        .env("QEMU_AUDIO", "none");
    if arch == RuntimeArch::Aarch64 {
        qemu_cmd.env("QEMU_FB", "auto");
        qemu_cmd.env("QEMU_VIRTIO_BUS", "mmio");
    }
    qemu_cmd.env("QEMU_INPUT", "virtio");
    qemu_cmd.env(
        "QEMU_AUDIO_WAV_PATH",
        format!("target/{}/debug/{smoke_tag}.wav", arch.kernel_target()),
    );
    let mut child = qemu_cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to start qemu run for {smoke_tag}"))?;

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
        wait_for_log(
            &log,
            "arrost /home/user",
            Duration::from_secs(40),
            "shell prompt",
        )?;
        let stdin = child
            .stdin
            .as_mut()
            .context("failed to capture qemu stdin")?;

        send_serial_command(stdin, "ring3\n")?;
        wait_for_log(
            &log,
            "ring3: mode=preemptive policy_smoke=available",
            Duration::from_secs(8),
            "ring3 status",
        )?;
        wait_for_log(
            &log,
            expected_transition,
            Duration::from_secs(8),
            "ring3 transition status",
        )?;

        send_serial_command(stdin, "ring3 smoke\n")?;
        wait_for_log(
            &log,
            "ring3(smoke):",
            Duration::from_secs(8),
            "ring3 smoke output",
        )?;
        wait_for_log(
            &log,
            "socket=1",
            Duration::from_secs(8),
            "ring3 socket ABI args smoke",
        )?;
        wait_for_log(
            &log,
            "sendto_bad_ptr=-22",
            Duration::from_secs(8),
            "ring3 sendto pointer validation",
        )?;
        wait_for_log(
            &log,
            "recvfrom_bad_ptr=-22",
            Duration::from_secs(8),
            "ring3 recvfrom pointer validation",
        )?;
        wait_for_log(
            &log,
            "time_after_drop=-1",
            Duration::from_secs(8),
            "ring3 time capability denial",
        )?;
        wait_for_log(
            &log,
            "result=ok",
            Duration::from_secs(8),
            "ring3 smoke pass result",
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
    if let Some(line) = last_matching_line(&log_snapshot, "ring3: mode=preemptive") {
        println!("{smoke_name}: {line}");
    }
    if let Some(line) = last_matching_line(&log_snapshot, "ring3(smoke):") {
        println!("{smoke_name}: {line}");
    }
    Ok(())
}

fn smoke_ring3_run(arch_override: Option<String>) -> Result<()> {
    let arch = resolve_runtime_arch(arch_override)?;
    let restore_force_fallback = env_truthy(DOOM_FORCE_FALLBACK_ENV);
    let restore_ring3_elf_groundwork = default_ring3_elf_groundwork_enabled();

    build_impl(restore_force_fallback, false, false, Some(true), false)?;
    let smoke_result = smoke_ring3_run_impl(arch);
    let restore_result = build_impl(
        restore_force_fallback,
        false,
        false,
        Some(restore_ring3_elf_groundwork),
        false,
    );
    match smoke_result {
        Ok(()) => {
            restore_result?;
            Ok(())
        }
        Err(smoke_err) => {
            if let Err(restore_err) = restore_result {
                return Err(smoke_err.context(format!(
                    "ring3 run smoke failed and restoring prior ELF groundwork state failed: {restore_err:#}"
                )));
            }
            Err(smoke_err)
        }
    }
}

fn smoke_ring3_run_impl(arch: RuntimeArch) -> Result<()> {
    ensure_runtime_artifacts(arch)?;

    let smoke_name = "smoke-ring3-run";
    let smoke_tag = format!("{smoke_name}-{}", arch.as_str());
    let launch_pattern = match arch {
        RuntimeArch::X86_64 => "ring3 run: entering user mode",
        RuntimeArch::Aarch64 => "ring3 run(a64): entering user mode",
    };
    let mut qemu_cmd = Command::new("bash");
    qemu_cmd
        .args([arch.qemu_script()])
        .env("QEMU_DISPLAY", "none")
        .env("QEMU_AUDIO", "none");
    if arch == RuntimeArch::Aarch64 {
        qemu_cmd.env("QEMU_FB", "auto");
        qemu_cmd.env("QEMU_VIRTIO_BUS", "mmio");
    }
    qemu_cmd.env("QEMU_INPUT", "virtio");
    qemu_cmd.env(
        "QEMU_AUDIO_WAV_PATH",
        format!("target/{}/debug/{smoke_tag}.wav", arch.kernel_target()),
    );
    let mut child = qemu_cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to start qemu run for {smoke_tag}"))?;

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
        wait_for_log(
            &log,
            "arrost /home/user",
            Duration::from_secs(40),
            "shell prompt",
        )?;
        let stdin = child
            .stdin
            .as_mut()
            .context("failed to capture qemu stdin")?;

        send_serial_command(stdin, "ring3 groundwork\n")?;
        wait_for_log(
            &log,
            "ring3(groundwork):",
            Duration::from_secs(12),
            "ring3 groundwork output",
        )?;
        wait_for_log(
            &log,
            "fd_badfd=-9",
            Duration::from_secs(8),
            "ring3 groundwork bad fd check",
        )?;
        wait_for_log(
            &log,
            "fd_emfile=-24",
            Duration::from_secs(8),
            "ring3 groundwork emfile check",
        )?;
        wait_for_log(
            &log,
            "fd=ok",
            Duration::from_secs(8),
            "ring3 groundwork fd smoke",
        )?;
        wait_for_log(
            &log,
            "result=ok",
            Duration::from_secs(8),
            "ring3 groundwork pass result",
        )?;

        send_serial_command(stdin, "ring3 run init\n")?;
        wait_for_log(
            &log,
            "ring3(run): queued app=init pid=",
            Duration::from_secs(8),
            "ring3 init queue acknowledgement",
        )?;
        send_serial_command(stdin, "ring3 run doom\n")?;
        wait_for_log(
            &log,
            "ring3(run): queued app=doom pid=",
            Duration::from_secs(8),
            "ring3 doom queue acknowledgement",
        )?;
        wait_for_log(
            &log,
            launch_pattern,
            Duration::from_secs(12),
            "ring3 run launch entry",
        )?;
        wait_for_log(
            &log,
            "nr=4 (yield)",
            Duration::from_secs(12),
            "ring3 init yield syscall",
        )?;
        wait_for_log(
            &log,
            "nr=5 (sleep)",
            Duration::from_secs(12),
            "ring3 init sleep syscall",
        )?;
        wait_for_log(
            &log,
            "nr=3 (exit) exit_code=7 -> kernel resume",
            Duration::from_secs(20),
            "ring3 init exit resume",
        )?;
        wait_for_log(
            &log,
            "nr=3 (exit) exit_code=11 -> kernel resume",
            Duration::from_secs(30),
            "ring3 doom exit resume",
        )?;

        let snapshot = snapshot_log(&log);
        if snapshot.contains("ring3(run): failed app=init") {
            bail!("ring3 run init reported failure in shell output");
        }
        if snapshot.contains("ring3(run): failed app=doom") {
            bail!("ring3 run doom reported failure in shell output");
        }

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
    if let Some(line) = last_matching_line(&log_snapshot, launch_pattern) {
        println!("{smoke_name}: {line}");
    }
    if let Some(line) = last_matching_line(&log_snapshot, "ring3(groundwork):") {
        println!("{smoke_name}: {line}");
    }
    if let Some(line) = last_matching_line(&log_snapshot, "nr=5 (sleep)") {
        println!("{smoke_name}: {line}");
    }
    if let Some(line) = last_matching_line(&log_snapshot, "nr=3 (exit) exit_code=7") {
        println!("{smoke_name}: {line}");
    }
    if let Some(line) = last_matching_line(&log_snapshot, "nr=3 (exit) exit_code=11") {
        println!("{smoke_name}: {line}");
    }
    Ok(())
}

fn smoke_ring3_fault(arch_override: Option<String>) -> Result<()> {
    let arch = resolve_runtime_arch(arch_override)?;
    if arch != RuntimeArch::Aarch64 {
        bail!("smoke-ring3-fault supports only --arch aarch64");
    }

    build_impl(false, true, true, None, false)?;
    let smoke_result = smoke_ring3_fault_impl(arch);
    let restore_result = build_impl(false, false, false, None, false);
    match smoke_result {
        Ok(()) => {
            restore_result?;
            Ok(())
        }
        Err(smoke_err) => {
            if let Err(restore_err) = restore_result {
                return Err(smoke_err.context(format!(
                    "ring3 fault smoke failed and restoring normal build failed: {restore_err:#}"
                )));
            }
            Err(smoke_err)
        }
    }
}

fn smoke_ring3_fault_impl(arch: RuntimeArch) -> Result<()> {
    ensure_runtime_artifacts(arch)?;

    let smoke_name = "smoke-ring3-fault";
    let smoke_tag = format!("{smoke_name}-{}", arch.as_str());
    let mut qemu_cmd = Command::new("bash");
    qemu_cmd
        .args([arch.qemu_script()])
        .env("QEMU_DISPLAY", "none")
        .env("QEMU_AUDIO", "none")
        .env("QEMU_FB", "auto")
        .env("QEMU_VIRTIO_BUS", "mmio")
        .env("QEMU_INPUT", "virtio")
        .env(
            "QEMU_AUDIO_WAV_PATH",
            format!("target/{}/debug/{smoke_tag}.wav", arch.kernel_target()),
        );
    let mut child = qemu_cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to start qemu run for {smoke_tag}"))?;

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
        wait_for_log(
            &log,
            "ring3 smoke(a64): entering user mode",
            Duration::from_secs(30),
            "ring3 fault smoke boot entry",
        )?;
        wait_for_log(
            &log,
            "mode=fault",
            Duration::from_secs(8),
            "ring3 fault mode marker",
        )?;
        wait_for_log(
            &log,
            "ring3 smoke(a64): lower-el sync fault",
            Duration::from_secs(20),
            "ring3 lower-el fault log",
        )?;
        wait_for_log(
            &log,
            "expected_fault=true",
            Duration::from_secs(8),
            "ring3 expected fault flag",
        )?;
        wait_for_log(
            &log,
            "result=expected_fault_hit",
            Duration::from_secs(8),
            "ring3 expected fault result",
        )?;
        wait_for_log(
            &log,
            "arrost /home/user",
            Duration::from_secs(30),
            "shell prompt after fault fallback resume",
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
    if let Some(line) = last_matching_line(&log_snapshot, "ring3 smoke(a64): lower-el sync fault") {
        println!("{smoke_name}: {line}");
    }
    Ok(())
}

fn smoke_kpti_m11() -> Result<()> {
    let archs = [RuntimeArch::X86_64, RuntimeArch::Aarch64];

    for arch in archs {
        println!("smoke-kpti-m11: running smoke-ring3 ({})", arch.as_str());
        smoke_ring3(Some(arch.as_str().to_owned()))?;

        println!(
            "smoke-kpti-m11: running smoke-ring3-run ({})",
            arch.as_str()
        );
        smoke_ring3_run(Some(arch.as_str().to_owned()))?;

        println!("smoke-kpti-m11: running smoke-fs ({})", arch.as_str());
        smoke_fs(Some(arch.as_str().to_owned()))?;
    }

    println!("smoke-kpti-m11: running smoke-ring3-fault (aarch64)");
    smoke_ring3_fault(Some(RuntimeArch::Aarch64.as_str().to_owned()))?;

    println!("smoke-kpti-m11: PASS");
    Ok(())
}

fn smoke_doom_impl(
    arch: RuntimeArch,
    long_run: bool,
    force_fallback: bool,
    strict_virtio: bool,
) -> Result<()> {
    ensure_runtime_artifacts(arch)?;

    let smoke_name = if strict_virtio {
        "smoke-doom-virtio"
    } else if force_fallback {
        "smoke-doom-fallback"
    } else if long_run {
        "smoke-doom-long"
    } else {
        "smoke-doom"
    };
    let smoke_tag = format!("{smoke_name}-{}", arch.as_str());

    let mut qemu_cmd = Command::new("bash");
    qemu_cmd
        .args([arch.qemu_script()])
        .env("QEMU_DISPLAY", "none");
    if arch == RuntimeArch::Aarch64 {
        qemu_cmd.env("QEMU_FB", "auto");
        qemu_cmd.env("QEMU_VIRTIO_BUS", "mmio");
    }
    if strict_virtio {
        qemu_cmd.env("QEMU_VIRTIO_SND", "on");
    }
    qemu_cmd.env("QEMU_INPUT", "virtio");
    if std::env::var_os("QEMU_AUDIO").is_none() {
        qemu_cmd.env("QEMU_AUDIO", "wav");
    }
    if std::env::var_os("QEMU_AUDIO_WAV_PATH").is_none() {
        qemu_cmd.env(
            "QEMU_AUDIO_WAV_PATH",
            format!("target/{}/debug/{smoke_tag}.wav", arch.kernel_target()),
        );
    }
    let mut child = qemu_cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to start qemu run for {smoke_tag}"))?;

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
        wait_for_log(
            &log,
            "arrost /home/user",
            Duration::from_secs(40),
            "shell prompt",
        )?;
        let startup_snapshot = snapshot_log(&log);
        let software_accel_mode = startup_snapshot.contains("Using QEMU acceleration: tcg")
            || startup_snapshot.contains("Using QEMU acceleration: none");
        let stdin = child
            .stdin
            .as_mut()
            .context("failed to capture qemu stdin")?;

        let ready = snapshot_log(&log).contains("DoomGeneric: ready=true");
        if force_fallback && ready {
            bail!("expected DoomGeneric ready=false for fallback smoke");
        }
        if !force_fallback && !ready {
            bail!(
                "doomgeneric ready=false in smoke-doom; run `cargo xtask build` (or wait for fallback restore) and retry"
            );
        }

        if arch == RuntimeArch::Aarch64 {
            wait_for_log(
                &log,
                "Input: backend=virtio-input-polled",
                Duration::from_secs(8),
                "aarch64 input readiness",
            )?;
            let boot_snapshot = snapshot_log(&log);
            if strict_virtio && !boot_snapshot.contains("Audio: backend=virtio-snd ready=true") {
                bail!("strict virtio smoke expected aarch64 audio backend=virtio-snd ready=true");
            }
            if !strict_virtio && !boot_snapshot.contains("Audio: backend=") {
                bail!("missing aarch64 audio backend report");
            }
            println!("smoke-doom: aarch64 headless boot readiness validated");
            return Ok(());
        }

        send_serial_command(stdin, "doom play\n")?;
        let play_marker = if ready {
            "doom: play mode started (doomgeneric)"
        } else {
            "doom: doomgeneric not ready; starting fallback runtime"
        };
        if wait_for_log(
            &log,
            play_marker,
            Duration::from_secs(12),
            "doom play confirmation",
        )
        .is_err()
        {
            send_serial_command(stdin, "doom play\n")?;
            wait_for_log(
                &log,
                play_marker,
                Duration::from_secs(12),
                "doom play confirmation (retry)",
            )?;
        }

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
            if !(fallback_line.contains("doomgeneric=false")
                || fallback_line.contains("bridge=stub"))
            {
                bail!("fallback status mismatch: expected bridge=stub or doomgeneric=false");
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
            // Escape capture and advance UI focus in the same serial burst so gfx poll
            // cannot immediately re-arm capture before the next shell command.
            send_serial_command(stdin, "\u{1b}ui next\n")?;
            wait_for_log(
                &log,
                "doom: capture disabled",
                Duration::from_secs(8),
                "doom auto-capture escape",
            )?;
            wait_for_log(
                &log,
                "ui: focus advanced",
                Duration::from_secs(8),
                "ui focus advance after auto-capture escape",
            )?;
        }

        wait_for_music_pcm_activity(&log, stdin, Duration::from_secs(10))?;

        send_serial_shell_command(stdin, "doom audio off\n")?;
        wait_for_log(
            &log,
            "doom: audio mode set to off",
            Duration::from_secs(8),
            "doom audio off",
        )?;

        send_serial_shell_command(stdin, "doom audio on\n")?;
        wait_for_log(
            &log,
            "doom: audio mode set to ",
            Duration::from_secs(8),
            "doom audio on",
        )?;

        if !long_run {
            send_serial_shell_command(stdin, "doom audio test\n")?;
            wait_for_log(
                &log,
                "doom: audio test tone queued",
                Duration::from_secs(8),
                "doom audio test",
            )?;
        }

        send_serial_shell_command(stdin, "doom capture on\n")?;
        wait_for_log(
            &log,
            "doom: capture enabled (press ESC to exit)",
            Duration::from_secs(8),
            "doom capture on",
        )?;

        send_serial_command(stdin, "ww  ww")?;
        thread::sleep(Duration::from_millis(220));

        send_serial_command(stdin, "\u{1b}ui next\n")?;
        wait_for_log(
            &log,
            "doom: capture disabled",
            Duration::from_secs(8),
            "doom capture escape",
        )?;
        wait_for_log(
            &log,
            "ui: focus advanced",
            Duration::from_secs(8),
            "ui focus advance after doom capture escape",
        )?;

        send_serial_shell_command(stdin, "doom status\n")?;
        wait_for_log(
            &log,
            "doom: app=doom engine=",
            Duration::from_secs(8),
            "doom status post-capture",
        )?;
        // The doom status line is very long (~500 bytes); wait for its final field so the
        // snapshot captures the complete line and parse_metric_value can find dg_key=.
        wait_for_log(
            &log,
            "last_key=",
            Duration::from_secs(4),
            "doom status line complete post-capture",
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

        send_serial_shell_command(stdin, "doom mouse y on\n")?;
        wait_for_log(
            &log,
            "doom: mouse y mapping enabled",
            Duration::from_secs(8),
            "doom mouse y on",
        )?;

        send_serial_shell_command(stdin, "doom mouse turn 5\n")?;
        wait_for_log(
            &log,
            "doom: mouse turn threshold set to 5",
            Duration::from_secs(8),
            "doom mouse turn",
        )?;

        send_serial_shell_command(stdin, "doom mouse move 7\n")?;
        wait_for_log(
            &log,
            "doom: mouse move threshold set to 7",
            Duration::from_secs(8),
            "doom mouse move",
        )?;

        send_serial_shell_command(stdin, "doom status\n")?;
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

        send_serial_shell_command(stdin, "doom key left\n")?;
        wait_for_log(
            &log,
            "doom: injected key 0x61",
            Duration::from_secs(8),
            "doom key injection",
        )?;

        send_serial_shell_command(stdin, "doom keyup left\n")?;
        wait_for_log(
            &log,
            "doom: injected keyup 0x61",
            Duration::from_secs(8),
            "doom keyup injection",
        )?;

        send_serial_shell_command(stdin, "doom key fire\n")?;
        wait_for_log(
            &log,
            "doom: injected key 0x20",
            Duration::from_secs(8),
            "doom fire injection",
        )?;
        thread::sleep(Duration::from_millis(220));

        send_serial_shell_command(stdin, "doom keyup fire\n")?;
        wait_for_log(
            &log,
            "doom: injected keyup 0x20",
            Duration::from_secs(8),
            "doom fire keyup injection",
        )?;

        send_serial_shell_command(stdin, "doom key enter\n")?;
        wait_for_log(
            &log,
            "doom: injected key 0x0a",
            Duration::from_secs(8),
            "doom enter injection",
        )?;

        send_serial_shell_command(stdin, "doom keyup enter\n")?;
        wait_for_log(
            &log,
            "doom: injected keyup 0x0a",
            Duration::from_secs(8),
            "doom enter keyup injection",
        )?;

        send_serial_shell_command(stdin, "doom status\n")?;
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

        send_serial_shell_command(stdin, "doom key right\n")?;
        wait_for_log(
            &log,
            "doom: injected key 0x64",
            Duration::from_secs(8),
            "doom right injection",
        )?;
        send_serial_shell_command(stdin, "doom keyup right\n")?;
        wait_for_log(
            &log,
            "doom: injected keyup 0x64",
            Duration::from_secs(8),
            "doom right keyup injection",
        )?;

        send_serial_shell_command(stdin, "doom status\n")?;
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
            // GitHub runners without KVM can show wider frame-rate variance under TCG.
            // Keep a lower floor in software emulation while retaining stricter checks on HW accel.
            let min_frame_progress = if software_accel_mode { 72u64 } else { 180u64 };
            let max_drop_delta = 4u64;

            thread::sleep(long_wait);
            send_serial_shell_command(stdin, "doom status\n")?;
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

        send_serial_shell_command(stdin, "ui\n")?;
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

        send_serial_shell_command(stdin, "doom stop\n")?;
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

fn env_bool(name: &str) -> Option<bool> {
    match std::env::var(name).ok()?.as_str() {
        "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON" => Some(true),
        "0" | "false" | "FALSE" | "no" | "NO" | "off" | "OFF" => Some(false),
        _ => None,
    }
}

fn default_ring3_elf_groundwork_enabled() -> bool {
    env_bool(RING3_ELF_GROUNDWORK_ENV).unwrap_or(true)
}

fn send_serial_command(stdin: &mut ChildStdin, command: &str) -> Result<()> {
    let normalized = command.replace('\n', "\r");
    stdin
        .write_all(normalized.as_bytes())
        .with_context(|| format!("failed to send command `{}`", command.trim_end()))?;
    stdin
        .flush()
        .with_context(|| format!("failed to flush command `{}`", command.trim_end()))?;
    Ok(())
}

fn send_serial_shell_command(stdin: &mut ChildStdin, command: &str) -> Result<()> {
    // Prefix ESC so commands still execute when Doom capture is auto-rearmed by UI focus.
    let mut prefixed = String::from("\u{1b}");
    prefixed.push_str(command);
    send_serial_command(stdin, &prefixed)
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

fn wait_for_line_after(
    log: &Arc<Mutex<Vec<u8>>>,
    marker: &str,
    expected_line: &str,
    timeout: Duration,
    stage: &str,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let snapshot = snapshot_log(log);
        let lines = lines_after_matching_until_prompt(&snapshot, marker);
        if lines.iter().any(|line| line.trim() == expected_line) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("timeout waiting for {stage}: expected `{expected_line}` after `{marker}`");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn wait_for_new_matching_line(
    log: &Arc<Mutex<Vec<u8>>>,
    marker: &str,
    previous_line: &str,
    timeout: Duration,
    stage: &str,
) -> Result<String> {
    let deadline = Instant::now() + timeout;
    loop {
        let snapshot = snapshot_log(log);
        if let Some(line) = last_matching_line(&snapshot, marker)
            && line != previous_line
        {
            return Ok(line.to_string());
        }
        if Instant::now() >= deadline {
            bail!("timeout waiting for {stage}: expected new line matching `{marker}`");
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
            && line.contains("last_key=")
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
        send_serial_shell_command(stdin, "doom audio status\n")?;
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

fn line_after_matching<'a>(log: &'a str, marker: &str) -> Option<&'a str> {
    let lines: Vec<&str> = log.lines().collect();
    for index in (0..lines.len()).rev() {
        if lines[index].contains(marker) {
            return lines.get(index + 1).copied();
        }
    }
    None
}

fn lines_after_matching_until_prompt<'a>(log: &'a str, marker: &str) -> Vec<&'a str> {
    let lines: Vec<&str> = log.lines().collect();
    for index in (0..lines.len()).rev() {
        if command_line_matches_marker(lines[index], marker) {
            let mut captured = Vec::new();
            for line in lines.iter().skip(index + 1) {
                let trimmed = line.trim();
                if trimmed.starts_with("user@arrost ") {
                    break;
                }
                if !trimmed.is_empty() {
                    captured.push(trimmed);
                }
            }
            return captured;
        }
    }
    for index in (0..lines.len()).rev() {
        if lines[index].contains(marker) {
            let mut captured = Vec::new();
            for line in lines.iter().skip(index + 1) {
                let trimmed = line.trim();
                if trimmed.starts_with("user@arrost ") {
                    break;
                }
                if !trimmed.is_empty() {
                    captured.push(trimmed);
                }
            }
            return captured;
        }
    }
    Vec::new()
}

fn command_line_matches_marker(line: &str, marker: &str) -> bool {
    let trimmed = line.trim();
    if trimmed == marker {
        return true;
    }
    if trimmed.starts_with("user@arrost ") {
        let mut suffix = String::from("> ");
        suffix.push_str(marker);
        return trimmed.ends_with(suffix.as_str());
    }
    false
}

fn parse_metric_value(line: &str, key: &str) -> Option<u64> {
    let start = line.find(key)?;
    let rest = &line[start + key.len()..];
    let value = rest
        .split(|ch: char| ch.is_whitespace() || ch == ',')
        .next()?;
    value.parse::<u64>().ok()
}

fn parse_signed_metric_value(line: &str, key: &str) -> Option<i64> {
    let start = line.find(key)?;
    let rest = &line[start + key.len()..];
    let value = rest
        .split(|ch: char| ch.is_whitespace() || ch == ',')
        .next()?;
    value.parse::<i64>().ok()
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

    reset_storage_disk_image()
}

fn reset_storage_disk_image() -> Result<PathBuf> {
    let disk_path = PathBuf::from(format!("target/{KERNEL_TARGET}/debug/m6-disk.img"));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn to_owned_args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn top_level_command_defaults_to_help_when_missing() {
        let parsed = parse_top_level_command(None).expect("empty command should map to help");
        assert!(parsed == TopLevelCommand::Help);
    }

    #[test]
    fn top_level_command_supports_help_aliases() {
        let short = parse_top_level_command(Some("-h")).expect("-h should map to help");
        assert!(short == TopLevelCommand::Help);

        let long = parse_top_level_command(Some("--help")).expect("--help should map to help");
        assert!(long == TopLevelCommand::Help);

        let explicit = parse_top_level_command(Some("help")).expect("help should map to help");
        assert!(explicit == TopLevelCommand::Help);
    }

    #[test]
    fn top_level_command_rejects_unknown_subcommand() {
        let error = match parse_top_level_command(Some("unknown")) {
            Ok(_) => panic!("unknown top-level command must fail"),
            Err(error) => error,
        };
        let message = format!("{error:#}");
        assert!(message.contains("unsupported xtask command"));
    }

    #[test]
    fn top_level_command_supports_ring3_smoke_subcommand() {
        let parsed = parse_top_level_command(Some("smoke-ring3"))
            .expect("smoke-ring3 should map to top-level command");
        assert!(parsed == TopLevelCommand::SmokeRing3);
    }

    #[test]
    fn top_level_command_supports_ring3_run_smoke_subcommand() {
        let parsed = parse_top_level_command(Some("smoke-ring3-run"))
            .expect("smoke-ring3-run should map to top-level command");
        assert!(parsed == TopLevelCommand::SmokeRing3Run);
    }

    #[test]
    fn top_level_command_supports_bin_exec_smoke_subcommand() {
        let parsed = parse_top_level_command(Some("smoke-bin-exec"))
            .expect("smoke-bin-exec should map to top-level command");
        assert!(parsed == TopLevelCommand::SmokeBinExec);
    }

    #[test]
    fn top_level_command_supports_fs_smoke_subcommand() {
        let parsed = parse_top_level_command(Some("smoke-fs"))
            .expect("smoke-fs should map to top-level command");
        assert!(parsed == TopLevelCommand::SmokeFs);
    }

    #[test]
    fn top_level_command_supports_ring3_fault_smoke_subcommand() {
        let parsed = parse_top_level_command(Some("smoke-ring3-fault"))
            .expect("smoke-ring3-fault should map to top-level command");
        assert!(parsed == TopLevelCommand::SmokeRing3Fault);
    }

    #[test]
    fn top_level_command_supports_kpti_m11_smoke_subcommand() {
        let parsed = parse_top_level_command(Some("smoke-kpti-m11"))
            .expect("smoke-kpti-m11 should map to top-level command");
        assert!(parsed == TopLevelCommand::SmokeKptiM11);
    }

    #[test]
    fn abi_check_arch_args_default_to_both_targets() {
        let parsed = parse_abi_check_arch_args(Vec::<String>::new().into_iter())
            .expect("default abi-check args should parse");
        assert!(parsed == vec![RuntimeArch::X86_64, RuntimeArch::Aarch64]);
    }

    #[test]
    fn abi_check_arch_args_preserve_order_and_dedup() {
        let parsed = parse_abi_check_arch_args(
            to_owned_args(&["--arch", "aarch64", "--arch", "x86_64", "--arch", "aarch64"])
                .into_iter(),
        )
        .expect("multi-arch abi-check args should parse");
        assert!(parsed == vec![RuntimeArch::Aarch64, RuntimeArch::X86_64]);
    }

    #[test]
    fn abi_check_arch_args_support_equals_syntax() {
        let parsed = parse_abi_check_arch_args(to_owned_args(&["--arch=x86_64"]).into_iter())
            .expect("--arch=<value> should parse");
        assert!(parsed == vec![RuntimeArch::X86_64]);
    }

    #[test]
    fn abi_check_arch_args_reject_unknown_flags() {
        let error = match parse_abi_check_arch_args(to_owned_args(&["--bad"]).into_iter()) {
            Ok(_) => panic!("unknown flags must fail"),
            Err(error) => error,
        };
        let message = format!("{error:#}");
        assert!(message.contains("unsupported argument"));
    }

    #[test]
    fn abi_check_arch_args_require_value_after_flag() {
        let error = match parse_abi_check_arch_args(to_owned_args(&["--arch"]).into_iter()) {
            Ok(_) => panic!("missing --arch value must fail"),
            Err(error) => error,
        };
        let message = format!("{error:#}");
        assert!(message.contains("missing value for --arch"));
    }

    #[test]
    fn run_arch_arg_supports_flag_and_equals_syntax() {
        let separate = parse_run_arch_arg(to_owned_args(&["--arch", "aarch64"]).into_iter())
            .expect("--arch value should parse");
        assert_eq!(separate, Some(String::from("aarch64")));

        let equals = parse_run_arch_arg(to_owned_args(&["--arch=x86_64"]).into_iter())
            .expect("--arch=<value> should parse");
        assert_eq!(equals, Some(String::from("x86_64")));
    }

    #[test]
    fn run_arch_arg_rejects_duplicate_arch_flag() {
        let error = match parse_run_arch_arg(
            to_owned_args(&["--arch", "x86_64", "--arch", "aarch64"]).into_iter(),
        ) {
            Ok(_) => panic!("duplicate --arch must fail"),
            Err(error) => error,
        };
        let message = format!("{error:#}");
        assert!(message.contains("duplicate --arch"));
    }

    #[test]
    fn run_arch_arg_rejects_unknown_flag() {
        let error = match parse_run_arch_arg(to_owned_args(&["--bad"]).into_iter()) {
            Ok(_) => panic!("unknown flag must fail"),
            Err(error) => error,
        };
        let message = format!("{error:#}");
        assert!(message.contains("unsupported argument"));
    }

    #[test]
    fn resolve_runtime_arch_accepts_aliases() {
        let amd64 =
            resolve_runtime_arch(Some(String::from("amd64"))).expect("amd64 alias should resolve");
        assert!(amd64 == RuntimeArch::X86_64);

        let arm64 =
            resolve_runtime_arch(Some(String::from("arm64"))).expect("arm64 alias should resolve");
        assert!(arm64 == RuntimeArch::Aarch64);
    }

    #[test]
    fn resolve_runtime_arch_rejects_unknown_value() {
        let error = match resolve_runtime_arch(Some(String::from("riscv64"))) {
            Ok(_) => panic!("unknown arch must fail"),
            Err(error) => error,
        };
        let message = format!("{error:#}");
        assert!(message.contains("unsupported arch"));
    }
}

#!/usr/bin/env bash
set -euo pipefail

EFI_DIR="${ARROST_AARCH64_ESP_DIR:-target/aarch64-unknown-none/debug/efi}"
DATA_IMG="target/x86_64-unknown-none/debug/m6-disk.img"
BOOT_EFI="$EFI_DIR/EFI/BOOT/BOOTAA64.EFI"
KERNEL_IMG="$EFI_DIR/arrost-kernel"

DEFAULT_CODE_CANDIDATES=(
  "/usr/share/AAVMF/AAVMF_CODE.fd"
  "/usr/share/AAVMF/AAVMF_CODE.ms.fd"
  "/opt/homebrew/share/qemu/edk2-aarch64-code.fd"
  "/usr/local/share/qemu/edk2-aarch64-code.fd"
  "/usr/share/qemu-efi-aarch64/QEMU_EFI.fd"
  "/usr/share/edk2/aarch64/QEMU_EFI.fd"
)
DEFAULT_VARS_CANDIDATES=(
  "/usr/share/AAVMF/AAVMF_VARS.fd"
  "/opt/homebrew/share/qemu/edk2-arm-vars.fd"
  "/usr/local/share/qemu/edk2-arm-vars.fd"
  "/usr/share/qemu-efi-aarch64/QEMU_VARS.fd"
)

resolve_first_existing() {
  for candidate in "$@"; do
    if [[ -f "$candidate" ]]; then
      echo "$candidate"
      return 0
    fi
  done
  return 1
}

if [[ -n "${AAVMF_CODE:-}" ]]; then
  AAVMF_CODE_PATH="$AAVMF_CODE"
else
  AAVMF_CODE_PATH="$(resolve_first_existing "${DEFAULT_CODE_CANDIDATES[@]}")"
fi

if [[ -n "${AAVMF_VARS:-}" ]]; then
  AAVMF_VARS_PATH="$AAVMF_VARS"
else
  AAVMF_VARS_TEMPLATE="$(resolve_first_existing "${DEFAULT_VARS_CANDIDATES[@]}")"
  AAVMF_VARS_PATH="target/aarch64-unknown-none/debug/aavmf-vars.fd"
  mkdir -p "$(dirname "$AAVMF_VARS_PATH")"
  if [[ ! -f "$AAVMF_VARS_PATH" ]]; then
    cp "$AAVMF_VARS_TEMPLATE" "$AAVMF_VARS_PATH"
  fi
fi

if [[ ! -f "$BOOT_EFI" ]]; then
  echo "Missing UEFI loader: $BOOT_EFI"
  echo "Run: cargo xtask build"
  exit 1
fi

if [[ ! -f "$KERNEL_IMG" ]]; then
  echo "Missing kernel image for UEFI loader: $KERNEL_IMG"
  echo "Run: cargo xtask build"
  exit 1
fi

if [[ ! -f "$DATA_IMG" ]]; then
  echo "Missing storage image: $DATA_IMG"
  echo "Run: cargo xtask build"
  exit 1
fi

if ! command -v qemu-system-aarch64 >/dev/null 2>&1; then
  echo "qemu-system-aarch64 not found in PATH"
  exit 1
fi

if [[ ! -d "$EFI_DIR" ]]; then
  echo "Missing UEFI ESP directory: $EFI_DIR"
  echo "Run: cargo xtask build"
  exit 1
fi

if [[ ! -f "$AAVMF_CODE_PATH" ]]; then
  echo "Missing AAVMF code firmware: $AAVMF_CODE_PATH"
  echo "Set AAVMF_CODE explicitly or install edk2/aavmf firmware files."
  exit 1
fi

if [[ ! -f "$AAVMF_VARS_PATH" ]]; then
  echo "Missing AAVMF vars firmware: $AAVMF_VARS_PATH"
  echo "Set AAVMF_VARS explicitly or install edk2/aavmf firmware files."
  exit 1
fi

AVAILABLE_ACCELS="$(qemu-system-aarch64 -accel help 2>/dev/null || true)"
accel_available() {
  local accel="$1"
  grep -Eq "(^|[[:space:]])${accel}($|[[:space:]])" <<<"$AVAILABLE_ACCELS"
}

pick_auto_accel() {
  if [[ "${OSTYPE:-}" == darwin* ]] && accel_available "hvf"; then
    echo "hvf"
    return 0
  fi
  if [[ "${OSTYPE:-}" == darwin* ]] && accel_available "tcg"; then
    echo "tcg"
    return 0
  fi
  if [[ "${OSTYPE:-}" == linux* ]] && accel_available "kvm"; then
    echo "kvm"
    return 0
  fi
  if accel_available "tcg"; then
    echo "tcg"
    return 0
  fi
  echo "none"
}

QEMU_ACCEL_MODE="${QEMU_ACCEL:-auto}"
if [[ "$QEMU_ACCEL_MODE" == "auto" ]]; then
  ACCEL_MODE="$(pick_auto_accel)"
elif [[ "$QEMU_ACCEL_MODE" == "none" ]]; then
  ACCEL_MODE="none"
elif accel_available "$QEMU_ACCEL_MODE"; then
  ACCEL_MODE="$QEMU_ACCEL_MODE"
else
  echo "Requested QEMU accelerator not available: $QEMU_ACCEL_MODE"
  echo "Falling back to auto acceleration selection."
  ACCEL_MODE="$(pick_auto_accel)"
fi

HVF_MODE_MARKER="$EFI_DIR/arr_hvf_mode"
if [[ "$ACCEL_MODE" == "hvf" ]]; then
  : >"$HVF_MODE_MARKER"
else
  rm -f "$HVF_MODE_MARKER"
fi

AVAILABLE_CPUS="$(qemu-system-aarch64 -cpu help 2>/dev/null | tr ', ' '\n' || true)"
cpu_available() {
  local cpu="$1"
  grep -qx "$cpu" <<<"$AVAILABLE_CPUS"
}

QEMU_CPU_MODE="${QEMU_CPU:-auto}"
if [[ "$QEMU_CPU_MODE" == "auto" ]]; then
  case "$ACCEL_MODE" in
    hvf | kvm)
      if cpu_available "host"; then
        CPU_MODEL="host"
      elif cpu_available "max"; then
        CPU_MODEL="max"
      else
        CPU_MODEL="cortex-a72"
      fi
      ;;
    *)
      if cpu_available "max"; then
        CPU_MODEL="max"
      else
        CPU_MODEL="cortex-a72"
      fi
      ;;
  esac
else
  CPU_MODEL="$QEMU_CPU_MODE"
  if ! cpu_available "$CPU_MODEL"; then
    echo "Requested CPU model not available: $CPU_MODEL"
    if cpu_available "max"; then
      CPU_MODEL="max"
    else
      CPU_MODEL="cortex-a72"
    fi
    echo "Falling back to CPU model: $CPU_MODEL"
  fi
fi

QEMU_SMP_MODE="${QEMU_SMP:-auto}"
if [[ "$QEMU_SMP_MODE" == "auto" ]]; then
  case "$ACCEL_MODE" in
    hvf | kvm)
      QEMU_SMP_CORES=2
      ;;
    *)
      QEMU_SMP_CORES=1
      ;;
  esac
else
  QEMU_SMP_CORES="$QEMU_SMP_MODE"
fi

if ! [[ "$QEMU_SMP_CORES" =~ ^[0-9]+$ ]] || [[ "$QEMU_SMP_CORES" -lt 1 ]]; then
  echo "Invalid QEMU_SMP value: $QEMU_SMP_CORES (using 1)"
  QEMU_SMP_CORES=1
fi

QEMU_MEM_SIZE="${QEMU_MEM:-512M}"

QEMU_VIRTIO_BUS_MODE_RAW="${QEMU_VIRTIO_BUS:-mmio}"
VIRTIO_BUS_MODE="mmio"
case "$QEMU_VIRTIO_BUS_MODE_RAW" in
  pci | pcie)
    echo "Requested QEMU virtio bus mode: $QEMU_VIRTIO_BUS_MODE_RAW"
    echo "aarch64 runtime currently supports only virtio-mmio; forcing bus mode: mmio"
    VIRTIO_BUS_MODE="mmio"
    ;;
  mmio | legacy-mmio)
    VIRTIO_BUS_MODE="mmio"
    ;;
  auto)
    VIRTIO_BUS_MODE="mmio"
    ;;
  *)
    echo "Requested QEMU virtio bus mode not recognized: $QEMU_VIRTIO_BUS_MODE_RAW"
    echo "Falling back to virtio bus mode: mmio"
    VIRTIO_BUS_MODE="mmio"
    ;;
esac

QEMU_VIRTIO_MMIO_MODE_RAW="${QEMU_VIRTIO_MMIO_MODE:-legacy}"
VIRTIO_MMIO_MODE="legacy"
VIRTIO_MMIO_GLOBAL_ARGS=()
if [[ "$VIRTIO_BUS_MODE" == "mmio" ]]; then
  case "$QEMU_VIRTIO_MMIO_MODE_RAW" in
    modern | v2)
      VIRTIO_MMIO_MODE="modern"
      VIRTIO_MMIO_GLOBAL_ARGS=(-global "virtio-mmio.force-legacy=false")
      ;;
    legacy | v1)
      VIRTIO_MMIO_MODE="legacy"
      VIRTIO_MMIO_GLOBAL_ARGS=(-global "virtio-mmio.force-legacy=true")
      ;;
    auto)
      VIRTIO_MMIO_MODE="auto"
      VIRTIO_MMIO_GLOBAL_ARGS=()
      ;;
    *)
      echo "Requested QEMU virtio-mmio mode not recognized: $QEMU_VIRTIO_MMIO_MODE_RAW"
      echo "Falling back to legacy virtio-mmio mode."
      VIRTIO_MMIO_MODE="legacy"
      VIRTIO_MMIO_GLOBAL_ARGS=(-global "virtio-mmio.force-legacy=true")
      ;;
  esac
else
  VIRTIO_MMIO_MODE="n/a"
  VIRTIO_MMIO_GLOBAL_ARGS=()
fi

AVAILABLE_DISPLAYS="$(qemu-system-aarch64 -display help 2>/dev/null || true)"
if [[ -n "${QEMU_DISPLAY:-}" ]]; then
  DISPLAY_BACKEND="$QEMU_DISPLAY"
else
  if grep -q "^cocoa$" <<<"$AVAILABLE_DISPLAYS"; then
    DISPLAY_BACKEND="cocoa"
  elif grep -q "^gtk$" <<<"$AVAILABLE_DISPLAYS"; then
    DISPLAY_BACKEND="gtk"
  elif grep -q "^sdl$" <<<"$AVAILABLE_DISPLAYS"; then
    DISPLAY_BACKEND="sdl"
  else
    DISPLAY_BACKEND="none"
  fi
fi

AVAILABLE_VIDEO_DEVICES="$(qemu-system-aarch64 -device help 2>/dev/null || true)"
video_device_available() {
  local name="$1"
  grep -q "name \"$name\"" <<<"$AVAILABLE_VIDEO_DEVICES"
}

QEMU_FB_MODE="${QEMU_FB:-auto}"
FRAMEBUFFER_DEVICE="none"
FRAMEBUFFER_ARGS=()

case "$QEMU_FB_MODE" in
  auto)
    if video_device_available "ramfb"; then
      FRAMEBUFFER_DEVICE="ramfb"
      FRAMEBUFFER_ARGS=(-device "ramfb")
    elif video_device_available "bochs-display"; then
      FRAMEBUFFER_DEVICE="bochs-display"
      FRAMEBUFFER_ARGS=(-device "bochs-display,xres=800,yres=600")
    fi
    ;;
  ramfb)
    if video_device_available "ramfb"; then
      FRAMEBUFFER_DEVICE="ramfb"
      FRAMEBUFFER_ARGS=(-device "ramfb")
    else
      echo "Requested framebuffer device not available: ramfb"
      echo "Falling back to auto framebuffer selection."
      if video_device_available "bochs-display"; then
        FRAMEBUFFER_DEVICE="bochs-display"
        FRAMEBUFFER_ARGS=(-device "bochs-display,xres=800,yres=600")
      fi
    fi
    ;;
  bochs | bochs-display)
    if video_device_available "bochs-display"; then
      FRAMEBUFFER_DEVICE="bochs-display"
      FRAMEBUFFER_ARGS=(-device "bochs-display,xres=800,yres=600")
    else
      echo "Requested framebuffer device not available: bochs-display"
      echo "Falling back to auto framebuffer selection."
      if video_device_available "ramfb"; then
        FRAMEBUFFER_DEVICE="ramfb"
        FRAMEBUFFER_ARGS=(-device "ramfb")
      fi
    fi
    ;;
  none | off)
    FRAMEBUFFER_DEVICE="none"
    FRAMEBUFFER_ARGS=()
    ;;
  *)
    echo "Requested framebuffer mode not recognized: $QEMU_FB_MODE"
    echo "Falling back to auto framebuffer selection."
    if video_device_available "ramfb"; then
      FRAMEBUFFER_DEVICE="ramfb"
      FRAMEBUFFER_ARGS=(-device "ramfb")
    elif video_device_available "bochs-display"; then
      FRAMEBUFFER_DEVICE="bochs-display"
      FRAMEBUFFER_ARGS=(-device "bochs-display,xres=800,yres=600")
    fi
    ;;
esac

AVAILABLE_AUDIO_DRIVERS="$(qemu-system-aarch64 -audiodev help 2>/dev/null || true)"
audio_driver_available() {
  local driver="$1"
  grep -q "^${driver}$" <<<"$AVAILABLE_AUDIO_DRIVERS"
}

QEMU_AUDIO_MODE="${QEMU_AUDIO:-auto}"
QEMU_VIRTIO_SND_MODE="${QEMU_VIRTIO_SND:-auto}"
QEMU_VIRTIO_SND_STREAMS="${QEMU_VIRTIO_SND_STREAMS:-1}"
AUDIO_BACKEND="none"
WAV_AUDIO_PATH=""
AUDIO_VOICE_ID="arr_audio0"
AUDIO_ARGS=()
VIRTIO_SOUND_ARGS=()

if [[ "$QEMU_AUDIO_MODE" == "none" ]]; then
  AUDIO_BACKEND="none"
elif [[ "$QEMU_AUDIO_MODE" == "auto" ]]; then
  if [[ "$DISPLAY_BACKEND" == "none" ]]; then
    AUDIO_BACKEND="none"
  elif audio_driver_available "coreaudio"; then
    AUDIO_BACKEND="coreaudio"
  elif audio_driver_available "wav"; then
    AUDIO_BACKEND="wav"
  else
    AUDIO_BACKEND="none"
  fi
elif audio_driver_available "$QEMU_AUDIO_MODE"; then
  AUDIO_BACKEND="$QEMU_AUDIO_MODE"
else
  echo "Requested QEMU audio backend not available: $QEMU_AUDIO_MODE"
  echo "Falling back to audio=none"
  AUDIO_BACKEND="none"
fi

if [[ "$AUDIO_BACKEND" != "none" ]]; then
  case "$AUDIO_BACKEND" in
    coreaudio)
      AUDIO_ARGS=(-audiodev "coreaudio,id=${AUDIO_VOICE_ID}")
      ;;
    wav)
      WAV_AUDIO_PATH="${QEMU_AUDIO_WAV_PATH:-target/aarch64-unknown-none/debug/qemu-audio.wav}"
      mkdir -p "$(dirname "$WAV_AUDIO_PATH")"
      AUDIO_ARGS=(-audiodev "wav,id=${AUDIO_VOICE_ID},path=$WAV_AUDIO_PATH")
      ;;
    *)
      AUDIO_BACKEND="none"
      AUDIO_ARGS=()
      ;;
  esac
fi

if [[ "$AUDIO_BACKEND" != "none" ]]; then
  if [[ "$QEMU_VIRTIO_SND_MODE" == "off" || "$QEMU_VIRTIO_SND_MODE" == "none" ]]; then
    VIRTIO_SOUND_ARGS=()
  else
    if [[ "$VIRTIO_BUS_MODE" == "pci" ]]; then
      VIRTIO_SOUND_ARGS=(-device "virtio-sound-pci,audiodev=${AUDIO_VOICE_ID},streams=${QEMU_VIRTIO_SND_STREAMS},disable-modern=off,disable-legacy=off")
    else
      VIRTIO_SOUND_ARGS=(-device "virtio-sound-device,audiodev=${AUDIO_VOICE_ID},streams=${QEMU_VIRTIO_SND_STREAMS}")
    fi
  fi
fi

UDP_FWD_PORT="${ARR_UDP_FWD_PORT:-}"
UDP_FWD_GUEST_PORT="${ARR_UDP_FWD_GUEST_PORT:-7777}"
TCP_FWD_PORT="${ARR_TCP_FWD_PORT:-}"
TCP_FWD_GUEST_PORT="${ARR_TCP_FWD_GUEST_PORT:-80}"
NETDEV_SPEC="user,id=arr_net"
if [[ -n "$UDP_FWD_PORT" ]]; then
  NETDEV_SPEC+=",hostfwd=udp::${UDP_FWD_PORT}-:${UDP_FWD_GUEST_PORT}"
fi
if [[ -n "$TCP_FWD_PORT" ]]; then
  NETDEV_SPEC+=",hostfwd=tcp::${TCP_FWD_PORT}-:${TCP_FWD_GUEST_PORT}"
fi

STORAGE_DEVICE_ARGS=()
ESP_DEVICE_ARGS=()
NET_DEVICE_ARGS=()
INPUT_DEVICE_ARGS=()
if [[ "$VIRTIO_BUS_MODE" == "pci" ]]; then
  STORAGE_DEVICE_ARGS=(-device "virtio-blk-pci,drive=arr_data,disable-modern=on,disable-legacy=off")
  ESP_DEVICE_ARGS=(
    -device "virtio-scsi-pci,id=arr_scsi,disable-modern=on,disable-legacy=off"
    -device "scsi-hd,drive=arr_esp,bus=arr_scsi.0,bootindex=0"
  )
  NET_DEVICE_ARGS=(-device "virtio-net-pci,netdev=arr_net,disable-modern=on,disable-legacy=off")
  INPUT_DEVICE_ARGS=(
    -device "virtio-keyboard-pci,disable-modern=on,disable-legacy=off"
    -device "virtio-mouse-pci,disable-modern=on,disable-legacy=off"
  )
else
  STORAGE_DEVICE_ARGS=(-device "virtio-blk-device,drive=arr_data")
  ESP_DEVICE_ARGS=(
    -device "virtio-scsi-device,id=arr_scsi"
    -device "scsi-hd,drive=arr_esp,bus=arr_scsi.0,bootindex=0"
  )
  NET_DEVICE_ARGS=(-device "virtio-net-device,netdev=arr_net")
  INPUT_DEVICE_ARGS=(
    -device "virtio-keyboard-device"
    -device "virtio-mouse-device"
  )
fi

QEMU_GIC_VERSION="${QEMU_GIC_VERSION:-2}"
if [[ "$QEMU_GIC_VERSION" != "2" && "$QEMU_GIC_VERSION" != "3" && "$QEMU_GIC_VERSION" != "max" ]]; then
  echo "Requested QEMU GIC version not recognized: $QEMU_GIC_VERSION (using 2)"
  QEMU_GIC_VERSION="2"
fi

echo "Using QEMU machine: virt (gic-version=$QEMU_GIC_VERSION)"
echo "Using QEMU display backend: $DISPLAY_BACKEND"
if [[ "$ACCEL_MODE" == "none" ]]; then
  echo "Using QEMU acceleration: none"
else
  echo "Using QEMU acceleration: $ACCEL_MODE"
fi
echo "Using QEMU CPU model: $CPU_MODEL"
echo "Using QEMU SMP cores: $QEMU_SMP_CORES"
echo "Using QEMU memory: $QEMU_MEM_SIZE"
echo "Using QEMU virtio bus: $VIRTIO_BUS_MODE"
echo "Using QEMU virtio-mmio mode: $VIRTIO_MMIO_MODE"
echo "Using QEMU audio backend: $AUDIO_BACKEND"
echo "Using QEMU framebuffer device: $FRAMEBUFFER_DEVICE"
echo "Using firmware code: $AAVMF_CODE_PATH"
echo "Using firmware vars: $AAVMF_VARS_PATH"
echo "Using UEFI ESP directory: $EFI_DIR"
if [[ "$ACCEL_MODE" == "hvf" ]]; then
  echo "Using UEFI handoff profile: hvf"
else
  echo "Using UEFI handoff profile: standard"
fi
if [[ ${#VIRTIO_SOUND_ARGS[@]} -gt 0 ]]; then
  echo "Using QEMU virtio-sound: on (streams=$QEMU_VIRTIO_SND_STREAMS)"
else
  echo "Using QEMU virtio-sound: off"
fi
if [[ -n "$WAV_AUDIO_PATH" ]]; then
  echo "Writing QEMU audio stream to: $WAV_AUDIO_PATH"
fi
if [[ -n "$UDP_FWD_PORT" ]]; then
  echo "Forwarding UDP host:${UDP_FWD_PORT} -> guest:${UDP_FWD_GUEST_PORT}"
fi
if [[ -n "$TCP_FWD_PORT" ]]; then
  echo "Forwarding TCP host:${TCP_FWD_PORT} -> guest:${TCP_FWD_GUEST_PORT}"
fi

QEMU_ARGS=(
  -machine "virt,gic-version=${QEMU_GIC_VERSION},secure=off"
  -cpu "$CPU_MODEL"
  -m "$QEMU_MEM_SIZE"
  -smp "$QEMU_SMP_CORES"
  -serial stdio
  -monitor none
  -drive if=pflash,format=raw,readonly=on,file="$AAVMF_CODE_PATH"
  -drive if=pflash,format=raw,file="$AAVMF_VARS_PATH"
)

if [[ ${#VIRTIO_MMIO_GLOBAL_ARGS[@]} -gt 0 ]]; then
  QEMU_ARGS+=("${VIRTIO_MMIO_GLOBAL_ARGS[@]}")
fi

QEMU_ARGS+=(
  -drive if=none,id=arr_data,format=raw,file="$DATA_IMG"
  "${STORAGE_DEVICE_ARGS[@]}"
  -drive if=none,id=arr_esp,format=raw,file=fat:rw:"$EFI_DIR"
  "${ESP_DEVICE_ARGS[@]}"
  -netdev "$NETDEV_SPEC"
  "${NET_DEVICE_ARGS[@]}"
  "${INPUT_DEVICE_ARGS[@]}"
)

if [[ ${#FRAMEBUFFER_ARGS[@]} -gt 0 ]]; then
  QEMU_ARGS+=("${FRAMEBUFFER_ARGS[@]}")
fi

if [[ "$ACCEL_MODE" != "none" ]]; then
  QEMU_ARGS+=(-accel "$ACCEL_MODE")
fi
if [[ ${#AUDIO_ARGS[@]} -gt 0 ]]; then
  QEMU_ARGS+=("${AUDIO_ARGS[@]}")
fi
if [[ ${#VIRTIO_SOUND_ARGS[@]} -gt 0 ]]; then
  QEMU_ARGS+=("${VIRTIO_SOUND_ARGS[@]}")
fi
QEMU_ARGS+=(-display "$DISPLAY_BACKEND")

exec qemu-system-aarch64 "${QEMU_ARGS[@]}"

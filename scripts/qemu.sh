#!/usr/bin/env bash
set -euo pipefail

# Immagine disco UEFI generata da xtask:
IMG="target/x86_64-unknown-none/debug/bootimage-arrost-kernel.bin"
DATA_IMG="target/x86_64-unknown-none/debug/m6-disk.img"

# Percorsi OVMF/EDK2 (variano per OS/distribuzione).
# Puoi sempre forzare i percorsi con OVMF_CODE e OVMF_VARS.
DEFAULT_CODE_CANDIDATES=(
  "/usr/share/OVMF/OVMF_CODE.fd"
  "/usr/share/OVMF/OVMF_CODE_4M.fd"
  "/opt/homebrew/share/qemu/edk2-x86_64-code.fd"
  "/usr/local/share/qemu/edk2-x86_64-code.fd"
)
DEFAULT_VARS_CANDIDATES=(
  "/usr/share/OVMF/OVMF_VARS.fd"
  "/usr/share/OVMF/OVMF_VARS_4M.fd"
  "/opt/homebrew/share/qemu/edk2-i386-vars.fd"
  "/usr/local/share/qemu/edk2-i386-vars.fd"
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

if [[ -n "${OVMF_CODE:-}" ]]; then
  OVMF_CODE_PATH="$OVMF_CODE"
else
  OVMF_CODE_PATH="$(resolve_first_existing "${DEFAULT_CODE_CANDIDATES[@]}")"
fi

if [[ -n "${OVMF_VARS:-}" ]]; then
  OVMF_VARS_PATH="$OVMF_VARS"
else
  OVMF_VARS_TEMPLATE="$(resolve_first_existing "${DEFAULT_VARS_CANDIDATES[@]}")"
  OVMF_VARS_PATH="target/x86_64-unknown-none/debug/ovmf-vars.fd"
  mkdir -p "$(dirname "$OVMF_VARS_PATH")"
  if [[ ! -f "$OVMF_VARS_PATH" ]]; then
    cp "$OVMF_VARS_TEMPLATE" "$OVMF_VARS_PATH"
  fi
fi

if [[ ! -f "$IMG" ]]; then
  echo "Missing image: $IMG"
  echo "Run: cargo xtask build"
  exit 1
fi

if [[ ! -f "$DATA_IMG" ]]; then
  echo "Missing storage image: $DATA_IMG"
  echo "Run: cargo xtask build"
  exit 1
fi

if lsof "$IMG" >/dev/null 2>&1; then
  echo "Image is already in use: $IMG"
  lsof "$IMG" || true
  echo "Close the running QEMU instance and retry."
  exit 1
fi

if [[ ! -f "$OVMF_CODE_PATH" ]]; then
  echo "Missing OVMF/EDK2 code firmware: $OVMF_CODE_PATH"
  echo "Set OVMF_CODE explicitly or install OVMF/edk2 firmware files."
  exit 1
fi

if [[ ! -f "$OVMF_VARS_PATH" ]]; then
  echo "Missing OVMF/EDK2 vars firmware: $OVMF_VARS_PATH"
  echo "Set OVMF_VARS explicitly or install OVMF/edk2 firmware files."
  exit 1
fi

# Display backend selection:
# - Override with QEMU_DISPLAY (e.g. cocoa, gtk, none, curses)
# - Otherwise pick the first backend supported by this QEMU build.
if [[ -n "${QEMU_DISPLAY:-}" ]]; then
  DISPLAY_BACKEND="$QEMU_DISPLAY"
else
  AVAILABLE_DISPLAYS="$(qemu-system-x86_64 -display help 2>/dev/null || true)"
  if grep -q "^cocoa$" <<<"$AVAILABLE_DISPLAYS"; then
    DISPLAY_BACKEND="cocoa"
  elif grep -q "^sdl$" <<<"$AVAILABLE_DISPLAYS"; then
    DISPLAY_BACKEND="sdl"
  elif grep -q "^gtk$" <<<"$AVAILABLE_DISPLAYS"; then
    DISPLAY_BACKEND="gtk"
  else
    DISPLAY_BACKEND="none"
  fi
fi

AVAILABLE_ACCELERATORS="$(qemu-system-x86_64 -accel help 2>/dev/null || true)"
accel_available() {
  local accel="$1"
  grep -Eq "(^|[[:space:]])${accel}($|[[:space:]])" <<<"$AVAILABLE_ACCELERATORS"
}

kvm_accessible() {
  [[ -c /dev/kvm ]] || return 1
  # Verify that this process can actually open /dev/kvm (CI may expose
  # the device node without allowing access to it).
  if exec {kvm_fd}<>/dev/kvm 2>/dev/null; then
    exec {kvm_fd}>&-
    return 0
  fi
  return 1
}

AVAILABLE_CPU_MODELS="$(qemu-system-x86_64 -cpu help 2>/dev/null || true)"
cpu_model_available() {
  local model="$1"
  grep -Eq "^[[:space:]]+${model}([[:space:]]|$)" <<<"$AVAILABLE_CPU_MODELS"
}

pick_auto_accel() {
  if [[ "${OSTYPE:-}" == darwin* ]] && accel_available "hvf"; then
    echo "hvf"
    return 0
  fi
  if [[ "${OSTYPE:-}" == linux* ]] && accel_available "kvm"; then
    if kvm_accessible; then
      echo "kvm"
      return 0
    fi
    if [[ -c /dev/kvm ]]; then
      echo "KVM unavailable: /dev/kvm is present but not accessible; falling back." >&2
    fi
  fi
  if accel_available "hvf"; then
    echo "hvf"
    return 0
  fi
  if accel_available "kvm" && kvm_accessible; then
    echo "kvm"
    return 0
  fi
  if accel_available "whpx"; then
    echo "whpx"
    return 0
  fi
  if accel_available "tcg"; then
    echo "tcg"
    return 0
  fi
  echo "none"
}

QEMU_ACCEL_MODE="${QEMU_ACCEL:-auto}"
QEMU_CPU_MODE="${QEMU_CPU:-auto}"
QEMU_SMP_MODE="${QEMU_SMP:-auto}"
QEMU_SMP_CORES="1"
ACCEL_MODE="none"
ACCEL_SPEC=""
CPU_SPEC="qemu64"

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

if [[ "$ACCEL_MODE" != "none" ]]; then
  ACCEL_SPEC="$ACCEL_MODE"
fi

if [[ "$QEMU_CPU_MODE" == "auto" ]]; then
  case "$ACCEL_MODE" in
    hvf | kvm | whpx)
      CPU_SPEC="host"
      ;;
    *)
      if cpu_model_available "max"; then
        CPU_SPEC="max"
      else
        CPU_SPEC="qemu64"
      fi
      ;;
  esac
else
  CPU_SPEC="$QEMU_CPU_MODE"
fi

if [[ "$CPU_SPEC" == "host" ]]; then
  case "$ACCEL_MODE" in
    hvf | kvm | whpx)
      ;;
    *)
      if cpu_model_available "max"; then
        echo "QEMU_CPU=host requires hardware acceleration; using max."
        CPU_SPEC="max"
      else
        echo "QEMU_CPU=host requires hardware acceleration; using qemu64."
        CPU_SPEC="qemu64"
      fi
      ;;
  esac
fi

if [[ -n "$CPU_SPEC" ]] && ! cpu_model_available "$CPU_SPEC"; then
  echo "Requested CPU model not available: $CPU_SPEC"
  if cpu_model_available "max"; then
    echo "Falling back to CPU model: max"
    CPU_SPEC="max"
  elif cpu_model_available "qemu64"; then
    echo "Falling back to CPU model: qemu64"
    CPU_SPEC="qemu64"
  else
    echo "Falling back to machine default CPU model."
    CPU_SPEC=""
  fi
fi

if [[ "$QEMU_SMP_MODE" == "auto" ]]; then
  case "$ACCEL_MODE" in
    hvf | kvm | whpx)
      QEMU_SMP_CORES=2
      ;;
    *)
      QEMU_SMP_CORES=1
      ;;
  esac
else
  QEMU_SMP_CORES="$QEMU_SMP_MODE"
fi

if ! [[ "$QEMU_SMP_CORES" =~ ^[0-9]+$ ]]; then
  echo "Invalid QEMU_SMP value: $QEMU_SMP_CORES (using 1)"
  QEMU_SMP_CORES=1
elif [[ "$QEMU_SMP_CORES" -lt 1 ]]; then
  echo "Invalid QEMU_SMP value: $QEMU_SMP_CORES (using 1)"
  QEMU_SMP_CORES=1
fi

AVAILABLE_AUDIO_DRIVERS="$(qemu-system-x86_64 -audiodev help 2>/dev/null || true)"
audio_driver_available() {
  local driver="$1"
  grep -q "^${driver}$" <<<"$AVAILABLE_AUDIO_DRIVERS"
}

# Audio backend selection:
# - Override with QEMU_AUDIO (auto|none|coreaudio|wav)
# - Optional wav output path with QEMU_AUDIO_WAV_PATH
# - Auto disables audio for headless display backend.
QEMU_AUDIO_MODE="${QEMU_AUDIO:-auto}"
QEMU_VIRTIO_SND_MODE="${QEMU_VIRTIO_SND:-auto}"
QEMU_VIRTIO_SND_STREAMS="${QEMU_VIRTIO_SND_STREAMS:-1}"
QEMU_PCSPK_MODE="${QEMU_PCSPK:-auto}"
MACHINE_SPEC="q35"
AUDIO_BACKEND="none"
WAV_AUDIO_PATH=""
AUDIO_VOICE_ID="arr_audio0"
AUDIO_ARGS=()
VIRTIO_SOUND_ARGS=()
PCSPK_ENABLED=0
VIRTIO_SOUND_ENABLED=0

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
      WAV_AUDIO_PATH="${QEMU_AUDIO_WAV_PATH:-target/x86_64-unknown-none/debug/qemu-audio.wav}"
      mkdir -p "$(dirname "$WAV_AUDIO_PATH")"
      AUDIO_ARGS=(-audiodev "wav,id=${AUDIO_VOICE_ID},path=$WAV_AUDIO_PATH")
      ;;
    *)
      echo "Unsupported QEMU audio backend in script: $AUDIO_BACKEND"
      echo "Falling back to audio=none"
      AUDIO_BACKEND="none"
      AUDIO_ARGS=()
      ;;
  esac
fi

if [[ "$AUDIO_BACKEND" != "none" ]]; then
  if [[ "$QEMU_VIRTIO_SND_MODE" == "off" || "$QEMU_VIRTIO_SND_MODE" == "none" ]]; then
    VIRTIO_SOUND_ENABLED=0
    VIRTIO_SOUND_ARGS=()
  else
    VIRTIO_SOUND_ENABLED=1
    VIRTIO_SOUND_ARGS=(-device "virtio-sound-pci,audiodev=${AUDIO_VOICE_ID},streams=${QEMU_VIRTIO_SND_STREAMS}")
  fi

  case "$QEMU_PCSPK_MODE" in
    on | true | 1)
      PCSPK_ENABLED=1
      ;;
    off | none | false | 0)
      PCSPK_ENABLED=0
      ;;
    auto)
      if [[ "$VIRTIO_SOUND_ENABLED" -eq 1 ]]; then
        PCSPK_ENABLED=0
      else
        PCSPK_ENABLED=1
      fi
      ;;
    *)
      echo "Unknown QEMU_PCSPK mode: $QEMU_PCSPK_MODE (using auto)"
      if [[ "$VIRTIO_SOUND_ENABLED" -eq 1 ]]; then
        PCSPK_ENABLED=0
      else
        PCSPK_ENABLED=1
      fi
      ;;
  esac

  if [[ "$PCSPK_ENABLED" -eq 1 ]]; then
    MACHINE_SPEC="q35,pcspk-audiodev=${AUDIO_VOICE_ID}"
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

NETDEV_ARGS=(-netdev "$NETDEV_SPEC")

echo "Using QEMU display backend: $DISPLAY_BACKEND"
if [[ "$ACCEL_MODE" == "none" ]]; then
  echo "Using QEMU acceleration: none"
else
  echo "Using QEMU acceleration: $ACCEL_SPEC"
fi
if [[ -n "$CPU_SPEC" ]]; then
  echo "Using QEMU CPU model: $CPU_SPEC"
else
  echo "Using QEMU CPU model: machine-default"
fi
echo "Using QEMU SMP cores: $QEMU_SMP_CORES"
echo "Using QEMU audio backend: $AUDIO_BACKEND"
if [[ ${#VIRTIO_SOUND_ARGS[@]} -gt 0 ]]; then
  echo "Using QEMU virtio-sound: on (streams=$QEMU_VIRTIO_SND_STREAMS)"
else
  echo "Using QEMU virtio-sound: off"
fi
if [[ "$AUDIO_BACKEND" != "none" ]]; then
  if [[ "$PCSPK_ENABLED" -eq 1 ]]; then
    echo "Using QEMU pc-speaker voice: on"
  else
    echo "Using QEMU pc-speaker voice: off"
  fi
fi
if [[ -n "$WAV_AUDIO_PATH" ]]; then
  echo "Writing QEMU audio stream to: $WAV_AUDIO_PATH"
fi
echo "Using firmware code: $OVMF_CODE_PATH"
echo "Using firmware vars: $OVMF_VARS_PATH"
if [[ -n "$UDP_FWD_PORT" ]]; then
  echo "Forwarding UDP host:${UDP_FWD_PORT} -> guest:${UDP_FWD_GUEST_PORT}"
fi
if [[ -n "$TCP_FWD_PORT" ]]; then
  echo "Forwarding TCP host:${TCP_FWD_PORT} -> guest:${TCP_FWD_GUEST_PORT}"
fi

QEMU_BASE_ARGS=(
  -machine "$MACHINE_SPEC"
)
if [[ -n "$ACCEL_SPEC" ]]; then
  QEMU_BASE_ARGS+=(-accel "$ACCEL_SPEC")
fi
QEMU_BASE_ARGS+=(
  -smp "$QEMU_SMP_CORES"
  -m 512M
  -serial stdio
  -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE_PATH"
  -drive if=pflash,format=raw,file="$OVMF_VARS_PATH"
  -drive format=raw,file="$IMG"
  -drive if=none,id=arr_data,format=raw,file="$DATA_IMG"
  -device virtio-blk-pci,drive=arr_data,disable-modern=on,disable-legacy=off
  "${NETDEV_ARGS[@]}"
  -device virtio-net-pci,netdev=arr_net,disable-modern=on,disable-legacy=off
)
if [[ -n "$CPU_SPEC" ]]; then
  QEMU_BASE_ARGS+=(-cpu "$CPU_SPEC")
fi

if [[ "$AUDIO_BACKEND" != "none" ]]; then
  exec qemu-system-x86_64 \
    "${QEMU_BASE_ARGS[@]}" \
    "${AUDIO_ARGS[@]}" \
    "${VIRTIO_SOUND_ARGS[@]}" \
    -display "$DISPLAY_BACKEND"
else
  exec qemu-system-x86_64 \
    "${QEMU_BASE_ARGS[@]}" \
    "${VIRTIO_SOUND_ARGS[@]}" \
    -display "$DISPLAY_BACKEND"
fi

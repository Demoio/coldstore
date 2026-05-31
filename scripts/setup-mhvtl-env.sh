#!/usr/bin/env bash
# Prepare a dedicated Linux VM/test host for mhVTL live tape-library testing.
#
# SAFETY DEFAULT: dry-run only. Nothing is installed unless --execute is passed.
# Do NOT run this on a normal development host unless you intentionally want to
# install kernel modules, systemd services, and virtual SCSI tape/changer devices.

set -euo pipefail

EXECUTE=0
START_SERVICES=0
MHVTL_REPO="${MHVTL_REPO:-https://github.com/markh794/mhvtl.git}"
MHVTL_SRC="${MHVTL_SRC:-/usr/local/src/mhvtl}"

usage() {
  cat <<'USAGE'
Usage:
  scripts/setup-mhvtl-env.sh [--execute] [--start-services]

Default mode is dry-run: commands are printed but not executed.

Options:
  --execute         Actually install packages, clone/build/install mhVTL.
  --start-services  After install, enable/start mhvtl.target via systemctl.
  -h, --help        Show this help.

Environment:
  MHVTL_REPO        Git repo to clone. Default: https://github.com/markh794/mhvtl.git
  MHVTL_SRC         Source checkout path. Default: /usr/local/src/mhvtl

Expected live validation after install:
  lsscsi -g
  ps -ax | grep '[v]tl'
  mtx -f /dev/sgX status        # use changer sg path from lsscsi output
  mt -f /dev/nstX status        # use non-rewinding tape path
  sg_inq /dev/sgX
  sg_turs /dev/sgX
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --execute) EXECUTE=1 ;;
    --start-services) START_SERVICES=1 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage; exit 2 ;;
  esac
  shift
done

if [[ $EXECUTE -eq 0 ]]; then
  echo "DRY-RUN mode. Pass --execute to actually modify this host." >&2
fi

SUDO=""
if [[ ${EUID:-$(id -u)} -ne 0 ]]; then
  SUDO="sudo"
fi

run() {
  printf '+ '
  printf '%q ' "$@"
  printf '\n'
  if [[ $EXECUTE -eq 1 ]]; then
    "$@"
  fi
}

run_sh() {
  printf '+ %s\n' "$*"
  if [[ $EXECUTE -eq 1 ]]; then
    bash -lc "$*"
  fi
}

require_linux() {
  if [[ "$(uname -s)" != "Linux" ]]; then
    echo "mhVTL requires Linux" >&2
    exit 1
  fi
}

install_packages() {
  if command -v apt-get >/dev/null 2>&1; then
    run $SUDO apt-get update
    run $SUDO apt-get install -y \
      build-essential git make gcc kmod pkg-config \
      "linux-headers-$(uname -r)" \
      lsscsi sg3-utils mtx mt-st \
      zlib1g-dev liblzo2-dev
  elif command -v dnf >/dev/null 2>&1; then
    run $SUDO dnf install -y \
      gcc gcc-c++ make git kmod kernel-devel kernel-headers \
      lsscsi sg3_utils mtx mt-st \
      zlib-devel lzo-devel
  elif command -v yum >/dev/null 2>&1; then
    run $SUDO yum install -y \
      gcc gcc-c++ make git kmod kernel-devel kernel-headers \
      lsscsi sg3_utils mtx mt-st \
      zlib-devel lzo-devel
  elif command -v zypper >/dev/null 2>&1; then
    run $SUDO zypper --non-interactive install \
      gcc gcc-c++ make git-core kmod kernel-default-devel \
      lsscsi sg3_utils mtx mt_st \
      zlib-devel lzo-devel
  elif command -v pacman >/dev/null 2>&1; then
    run $SUDO pacman -Sy --needed --noconfirm \
      base-devel git linux-headers kmod \
      lsscsi sg3_utils mtx mt-st \
      zlib lzo
  else
    echo "unsupported package manager; install build tools, kernel headers, lsscsi, sg3_utils, mtx, mt-st, zlib-dev, lzo-dev manually" >&2
    exit 1
  fi
}

checkout_mhvtl() {
  if [[ -d "$MHVTL_SRC/.git" ]]; then
    run git -C "$MHVTL_SRC" fetch --all --tags --prune
  else
    run $SUDO mkdir -p "$(dirname "$MHVTL_SRC")"
    if [[ -n "$SUDO" ]]; then
      run $SUDO git clone "$MHVTL_REPO" "$MHVTL_SRC"
    else
      run git clone "$MHVTL_REPO" "$MHVTL_SRC"
    fi
  fi
}

build_and_install_mhvtl() {
  # Upstream layouts have changed over time. Build the kernel module directory
  # explicitly when present, then run the top-level build/install.
  if [[ -d "$MHVTL_SRC/kernel" ]]; then
    run make -C "$MHVTL_SRC/kernel"
    run $SUDO make -C "$MHVTL_SRC/kernel" install
  fi

  run make -C "$MHVTL_SRC"
  run $SUDO make -C "$MHVTL_SRC" install
  run $SUDO systemctl daemon-reload
}

start_services() {
  if [[ $START_SERVICES -eq 1 ]]; then
    run $SUDO systemctl enable mhvtl.target
    run $SUDO systemctl start mhvtl.target
    run $SUDO systemctl status --no-pager mhvtl.target
  else
    echo "Skipping service start. Pass --start-services to enable/start mhvtl.target." >&2
  fi
}

print_next_steps() {
  cat <<'NEXT'

Next manual checks on the dedicated VM/test host:
  lsscsi -g
  ps -ax | grep '[v]tl'

Pick paths from lsscsi output:
  mediumx ... /dev/sch0 /dev/sgCHANGER
  tape    ... /dev/stN  /dev/sgTAPE

Then test command paths:
  mtx -f /dev/sgCHANGER status
  mt -f /dev/nstN status
  sg_inq /dev/sgTAPE
  sg_turs /dev/sgTAPE

Do not point ColdStore live tests at production tape devices.
NEXT
}

require_linux
install_packages
checkout_mhvtl
build_and_install_mhvtl
start_services
print_next_steps

#!/usr/bin/env bash
# Install / refresh the dnp3_master_io_v3 systemd unit.
# Run with: sudo bash deploy/install_systemd.sh
#
# Idempotent: removes any old v2 unit, drops the v3 unit in, reloads,
# and enables it. Re-run safely after editing the .service file.

set -euo pipefail

if [[ ${EUID} -ne 0 ]]; then
    echo "must run as root: sudo bash $0" >&2
    exit 1
fi

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
UNIT_SRC="${REPO_DIR}/deploy/systemd/dnp3_master_io_v3.service"
UNIT_DST="/etc/systemd/system/dnp3_master_io_v3.service"
OLD_UNIT="/etc/systemd/system/dnp3_master_io_v2.service"

if [[ ! -f "${UNIT_SRC}" ]]; then
    echo "missing ${UNIT_SRC}" >&2
    exit 1
fi

# Retire any v2 unit if still present.
if systemctl list-unit-files dnp3_master_io_v2.service --no-pager >/dev/null 2>&1; then
    systemctl disable --now dnp3_master_io_v2.service 2>/dev/null || true
fi
rm -f "${OLD_UNIT}"

cp "${UNIT_SRC}" "${UNIT_DST}"
systemctl daemon-reload
systemctl enable --now dnp3_master_io_v3.service

echo
systemctl status dnp3_master_io_v3.service --no-pager -l || true

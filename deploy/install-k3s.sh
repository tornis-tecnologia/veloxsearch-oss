#!/usr/bin/env bash
# Copyright (C) 2026 Tornis Desenvolvimento
# SPDX-License-Identifier: AGPL-3.0-only
#
# From-zero installer: a bare Linux VM -> k3s + Longhorn + VeloxSearch (#189).
#
# The route for people with no Kubernetes yet. On a cluster you already have,
# apply the release's install.yaml instead (docs/INSTALL.md) — this script is
# that same apply with the cluster built underneath it:
#
#   server (default)  Longhorn node packages -> k3s server (pinned) without its
#                     node-local `local-path` storage -> the Longhorn bundle the
#                     app itself vendors (deploy/bootstrap/longhorn.yaml), so
#                     `longhorn` is the only default StorageClass -> the release
#                     install.yaml -> wait for the app -> print the URL and the
#                     command that joins more machines.
#   --join            Longhorn node packages -> k3s agent joining that server.
#
# Everything past that — cert-manager, the OpenSearch operator, Longhorn replica
# sizing for 1-2 node clusters — is the app's own bootstrap (src/bootstrap.rs),
# exactly as on any other cluster. Every download is pinned: the k3s installer
# by tag and sha256 (it then checks the k3s binary against the k3s release's
# own checksums), the Longhorn bundle by sha256, install.yaml against the
# release's SHA256SUMS.
#
# Re-running is safe: a step that is already done is skipped, and manifests are
# re-applied in place. It never upgrades k3s. Usage: install-k3s.sh --help
set -euo pipefail

REPO="${VELOX_REPO:-tornis-tecnologia/veloxsearch-oss}"
# The release this script installs by default. CI keeps it equal to the
# Cargo.toml version, as it does install.yaml's image tag.
VELOX_VERSION=0.15.0
# k3s line the conformance fleet runs (docs/REQUIREMENTS.md). The installer
# script is fetched at the same tag and checked against this digest.
K3S_VERSION=v1.36.6+k3s1
K3S_INSTALLER_SHA256=46177d4c99440b4c0311b67233823a8e8a2fc09693f6c89af1a7161e152fbfad
# sha256 of deploy/bootstrap/longhorn.yaml (Longhorn v1.8.1 + the ADR-046 disk
# headroom). CI fails when the bundle changes and this does not.
LONGHORN_SHA256=3fe04e7245d5528ae7679999287d86ade02ee193b7dcd3dc7c051ce18495c7b2
# The field manager the app applies its bundles with, so Longhorn installed
# here is indistinguishable from Longhorn the app installed itself.
FIELD_MANAGER=veloxsearch-bootstrap
APP_NS=veloxsearch-system
KUBECONFIG_PATH=/etc/rancher/k3s/k3s.yaml
TOKEN_FILE=/var/lib/rancher/k3s/server/node-token

JOIN="" TOKEN="${K3S_TOKEN:-}" UNINSTALL=0

usage() {
  cat <<EOF
Usage: sudo $0 [options]

First machine (k3s server + Longhorn + VeloxSearch):
  --version X.Y.Z     VeloxSearch release to install (default: ${VELOX_VERSION})

Every further machine (k3s agent):
  --join SERVER       the first machine's address (IP, host, or https://host:6443)
  --token TOKEN       the join token the first machine printed; prefer the
                      K3S_TOKEN environment variable, which stays out of
                      'ps'. Asked for interactively when neither is given.

Remove:
  --uninstall         run k3s's own uninstaller (server or agent); this
                      deletes the cluster and everything in it

Environment: VELOX_REPO (owner/name, default ${REPO}), K3S_TOKEN.
EOF
}

say()  { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m!!\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31mxx\033[0m %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --version) VELOX_VERSION="${2:?--version needs X.Y.Z}"; VELOX_VERSION="${VELOX_VERSION#v}"; shift 2 ;;
    --join) JOIN="${2:?--join needs the server address}"; shift 2 ;;
    --token) TOKEN="${2:?--token needs a value}"; shift 2 ;;
    --uninstall) UNINSTALL=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown option: $1" ;;
  esac
done

[ "$(id -u)" -eq 0 ] || die "run as root (sudo $0 ...)"
[ "$(uname -s)" = Linux ] || die "Linux only"
case "$(uname -m)" in
  x86_64|amd64) ;;
  *) die "unsupported architecture $(uname -m): VeloxSearch images are amd64 only (docs/REQUIREMENTS.md)" ;;
esac

have() { command -v "$1" >/dev/null 2>&1; }

# ---------------------------------------------------------------- uninstall
if [ "$UNINSTALL" -eq 1 ]; then
  for u in /usr/local/bin/k3s-uninstall.sh /usr/local/bin/k3s-agent-uninstall.sh; do
    if [ -x "$u" ]; then
      say "running $u"
      "$u"
      say "k3s removed. Longhorn replica data stays in /var/lib/longhorn: delete it to reclaim the disk"
      exit 0
    fi
  done
  die "no k3s uninstaller found: k3s is not installed on this machine"
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

fetch() { # url dest
  if have curl; then curl -fsSL --retry 3 --proto '=https' -o "$2" "$1"
  elif have wget; then wget -q -O "$2" "$1"
  else die "need curl or wget"
  fi
}

sha256_is() { # file expected
  [ "$(sha256sum "$1" | cut -d' ' -f1)" = "$2" ]
}

# ------------------------------------------------------------------ the host
[ -r /etc/os-release ] || die "/etc/os-release not found: cannot tell the distribution"
os_field() { # KEY — sourced in a subshell so os-release cannot clobber ours
  # shellcheck disable=SC1091
  (. /etc/os-release && eval "printf '%s' \"\${$1:-}\"")
}
OS_ID="$(os_field ID)"
case " $OS_ID $(os_field ID_LIKE) " in
  *" debian "*|*" ubuntu "*) FAMILY=debian ;;
  *" rhel "*|*" fedora "*|*" centos "*) FAMILY=rhel ;;
  *) die "unsupported distribution '${OS_ID}': this script knows apt and dnf hosts. Install k3s and the Longhorn node packages yourself, then apply install.yaml (docs/INSTALL.md)" ;;
esac
say "host: ${OS_ID} $(os_field VERSION_ID), $(awk '/MemTotal/ { printf "%.1f GiB RAM", $2 / 1048576 }' /proc/meminfo)"

# Longhorn's node prerequisites (docs/INSTALL.md §1): an iSCSI initiator with
# iscsid running, an NFS client (RWX volumes) and dmsetup. Every node needs
# them, server and agents alike.
longhorn_prereqs() {
  local missing=0
  have iscsiadm || missing=1
  [ -x /sbin/mount.nfs ] || [ -x /usr/sbin/mount.nfs ] || missing=1
  have dmsetup || missing=1
  if [ "$missing" -eq 1 ]; then
    say "installing the Longhorn node packages (iSCSI initiator, NFS client, dmsetup)"
    case "$FAMILY" in
      debian)
        apt-get update -q
        DEBIAN_FRONTEND=noninteractive apt-get install -y -q open-iscsi nfs-common dmsetup ;;
      rhel)
        if have dnf; then dnf install -y -q iscsi-initiator-utils nfs-utils device-mapper
        else yum install -y -q iscsi-initiator-utils nfs-utils device-mapper; fi ;;
    esac
  else
    say "Longhorn node packages already installed"
  fi
  systemctl enable --now iscsid >/dev/null 2>&1 || die "could not start iscsid"
  # Minimal cloud kernels can ship without the iSCSI transport; Longhorn then
  # cannot attach a single volume. Better to stop here than at the first PVC.
  modprobe iscsi_tcp 2>/dev/null \
    || die "the kernel has no iscsi_tcp module ($(uname -r)): install the distribution's full kernel (or its extra-modules package), reboot, re-run"
}

# k3s's own installer, at the pinned tag and digest. Arguments go through as
# environment (INSTALL_K3S_EXEC, K3S_URL, K3S_TOKEN) the way k3s documents.
install_k3s() {
  local url="https://raw.githubusercontent.com/k3s-io/k3s/${K3S_VERSION/+/%2B}/install.sh"
  fetch "$url" "$TMP/k3s-install.sh"
  sha256_is "$TMP/k3s-install.sh" "$K3S_INSTALLER_SHA256" \
    || die "k3s installer at ${K3S_VERSION} does not match its pinned sha256: refusing to run it"
  INSTALL_K3S_VERSION="$K3S_VERSION" sh "$TMP/k3s-install.sh"
}

# k3s's installer writes its unit here: k3s.service (server), k3s-agent.service.
k3s_installed() { [ -x /usr/local/bin/k3s ] && [ -f "/etc/systemd/system/$1.service" ]; }

# --------------------------------------------------------------------- join
if [ -n "$JOIN" ]; then
  case "$JOIN" in
    https://*) SERVER_URL="$JOIN" ;;
    *:*) SERVER_URL="https://$JOIN" ;;
    *) SERVER_URL="https://$JOIN:6443" ;;
  esac
  if [ -z "$TOKEN" ]; then
    [ -t 0 ] || die "no join token: pass K3S_TOKEN=... or --token (the server printed it)"
    printf 'Join token (printed by the server install): '
    stty -echo; read -r TOKEN; stty echo; printf '\n'
    [ -n "$TOKEN" ] || die "empty join token"
  fi
  longhorn_prereqs
  if k3s_installed k3s-agent; then
    say "k3s agent already installed ($(/usr/local/bin/k3s --version | head -1)): leaving it as is"
  elif k3s_installed k3s; then
    die "this machine already runs a k3s server: --join is for a fresh machine"
  else
    say "installing the k3s ${K3S_VERSION} agent, joining ${SERVER_URL}"
    K3S_URL="$SERVER_URL" K3S_TOKEN="$TOKEN" install_k3s
  fi
  systemctl is-active --quiet k3s-agent || die "k3s-agent is not running: journalctl -u k3s-agent"
  say "joined. On the server: kubectl get nodes   (this node shows Ready within a minute)"
  exit 0
fi

# ------------------------------------------------------------------- server
mem_kib="$(awk '/MemTotal/ { print $2 }' /proc/meminfo)"
if [ "$mem_kib" -lt 7800000 ]; then
  warn "less than 8 GiB of RAM: VeloxSearch needs 8 GiB schedulable across the cluster (R4) — join more machines before creating a deployment"
fi

longhorn_prereqs

if k3s_installed k3s; then
  say "k3s already installed ($(/usr/local/bin/k3s --version | head -1)): leaving it as is"
else
  # local-storage off: its `local-path` class would be the cluster default,
  # and a node-local default is what the app rides when it finds one
  # (ADR-061). Longhorn below becomes the only default instead.
  say "installing k3s ${K3S_VERSION} (server)"
  INSTALL_K3S_EXEC="server --disable=local-storage" install_k3s
fi

export KUBECONFIG="$KUBECONFIG_PATH"
kc() { /usr/local/bin/k3s kubectl "$@"; }

say "waiting for the node to be Ready"
for _ in $(seq 60); do
  kc wait --for=condition=Ready node --all --timeout=5s >/dev/null 2>&1 && break
  sleep 3
done
kc wait --for=condition=Ready node --all --timeout=5s >/dev/null || die "node not Ready after 3 minutes: journalctl -u k3s"

RAW="https://raw.githubusercontent.com/${REPO}/v${VELOX_VERSION}"
REL="https://github.com/${REPO}/releases/download/v${VELOX_VERSION}"

if [ "$(kc get storageclass longhorn -o jsonpath='{.provisioner}' 2>/dev/null)" = driver.longhorn.io ]; then
  say "Longhorn already installed"
else
  say "installing Longhorn (the bundle VeloxSearch v${VELOX_VERSION} vendors)"
  fetch "$RAW/deploy/bootstrap/longhorn.yaml" "$TMP/longhorn.yaml" \
    || die "could not download the Longhorn bundle of v${VELOX_VERSION}: is that a published release?"
  sha256_is "$TMP/longhorn.yaml" "$LONGHORN_SHA256" \
    || die "the Longhorn bundle of v${VELOX_VERSION} is not the one this script pins: use install-k3s.sh from that release"
  kc apply --server-side --force-conflicts --field-manager="$FIELD_MANAGER" -f "$TMP/longhorn.yaml" >/dev/null
  kc -n longhorn-system rollout status daemonset/longhorn-manager --timeout=600s
  kc -n longhorn-system rollout status deployment/longhorn-driver-deployer --timeout=600s
  # The driver-deployer creates the class once the CSI driver registers.
  for _ in $(seq 100); do
    kc get storageclass longhorn >/dev/null 2>&1 && break
    sleep 6
  done
  kc get storageclass longhorn >/dev/null 2>&1 \
    || die "Longhorn did not create its StorageClass in 10 minutes: kubectl -n longhorn-system get pods"
fi

say "installing VeloxSearch v${VELOX_VERSION}"
fetch "$REL/install.yaml" "$TMP/install.yaml" \
  || die "could not download install.yaml of v${VELOX_VERSION}: is that a published release?"
fetch "$REL/SHA256SUMS" "$TMP/SHA256SUMS" || die "could not download SHA256SUMS of v${VELOX_VERSION}"
(cd "$TMP" && sha256sum --ignore-missing -c SHA256SUMS >/dev/null) \
  || die "install.yaml does not match the release's SHA256SUMS: refusing to apply it"
kc apply -f "$TMP/install.yaml" >/dev/null
kc -n "$APP_NS" rollout status deployment/veloxsearch --timeout=600s

ip="$(ip -4 route get 1.1.1.1 2>/dev/null | sed -n 's/.* src \([0-9.]*\).*/\1/p')"
ip="${ip:-<this-machine-ip>}"
say "VeloxSearch v${VELOX_VERSION} is up on k3s ${K3S_VERSION} with Longhorn"
cat <<EOF

  Open        http://${ip}:30080/   (also http://${ip}/ through k3s's Traefik)
  First run   the first visit opens /setup: you create the admin account
              there — there is no generated password to look up.
  kubectl     export KUBECONFIG=${KUBECONFIG_PATH}   (root only)

  Add a machine — run on it, as root:
    curl -fsSLO https://raw.githubusercontent.com/${REPO}/v${VELOX_VERSION}/deploy/install-k3s.sh
    K3S_TOKEN='$(cat "$TOKEN_FILE")' bash install-k3s.sh --join ${ip}

  Remove everything: bash install-k3s.sh --uninstall (on each machine)

EOF

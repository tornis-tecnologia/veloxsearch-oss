#!/usr/bin/env bash
# Copyright (C) 2026 Tornis Desenvolvimento
# SPDX-License-Identifier: AGPL-3.0-only
#
# VeloxSearch full installer: bare Linux servers -> a three-node HA k3s
# cluster -> Longhorn -> VeloxSearch, ready for the first-run /setup.
#
# VeloxSearch itself installs INTO a cluster (docs/INSTALL.md §0); this script
# is the "no cluster yet" path done properly for production, so it owns the
# three things the manual path leaves to the operator:
#
#   1. Every node's prerequisites — the packages Longhorn needs (open-iscsi,
#      NFS client, cryptsetup; R3, #15), the kernel modules, the sysctls
#      OpenSearch needs (vm.max_map_count), swap off, the multipathd
#      blacklist Longhorn's KB asks for, and the firewall ports k3s needs.
#   2. k3s in HA: three servers on embedded etcd (quorum survives losing one
#      node), version pinned, secrets encrypted at rest, scheduled etcd
#      snapshots, and the node-local `local-path` provisioner DISABLED — a
#      node-local default makes VeloxSearch use it instead of replicated
#      storage (ADR-061), and k3s's addon reconciler restores it as a second
#      default after anyone demotes it (the app re-demotes on an interval,
#      #133/#134; disabling it leaves nothing to race).
#   3. Longhorn installed BEFORE VeloxSearch, from the exact bundle that
#      VeloxSearch release vendors (deploy/bootstrap/longhorn.yaml, so the app
#      sees no drift and reconciles it as its own), as the cluster's sole
#      default StorageClass — so even the control-plane Postgres lands on
#      three replicas instead of one node's disk.
#
# Then the release's digest-pinned install.yaml, checked against SHA256SUMS
# (and the image against its cosign signature when cosign is installed).
#
# What it deliberately does NOT do: create the VeloxSearch admin. That
# password is chosen at http://<node>/setup by whoever installs, and never
# passes through a script, an argument or a log. cert-manager and the
# OpenSearch operator are installed by the app at first run (bootstrap.rs),
# exactly as on any other cluster.
#
# Two ways to drive it:
#
#   One command, from the first node, the others reached over SSH (key-based,
#   with passwordless sudo or as root):
#     sudo ./install-veloxsearch-full.sh install --peers 10.0.0.12,10.0.0.13
#
#   Or node by node, no SSH between them:
#     node 1:  sudo ./install-veloxsearch-full.sh init
#     node 2:  sudo ./install-veloxsearch-full.sh join --server https://10.0.0.11:6443 --token-file token
#     node 3:  (the same)
#     node 1:  sudo ./install-veloxsearch-full.sh platform
#
# Every phase checks before it acts, so re-running is safe. Usage: --help
set -euo pipefail

REPO="${VELOX_REPO:-tornis-tecnologia/veloxsearch-oss}"
# The k3s line the conformance fleet runs (docs/REQUIREMENTS.md: 3-node k3s
# with a real Longhorn default, k3s v1.36.x). Pinned, not "stable": a fresh
# install six months from now must get the cluster that was tested, and the
# vendored Longhorn (v1.8.1) is only claimed for Kubernetes versions we ran.
K3S_VERSION_DEFAULT="v1.36.4+k3s1"
IMAGE_REPO="docker.io/tornistecnologia/veloxsearch-oss"
NS=veloxsearch-system
# The field manager VeloxSearch's own bootstrap applies Longhorn with
# (bootstrap.rs BOOTSTRAP_BINDING): the same owner, so the app's later
# reconcile of the bundle is an update, not a conflict.
FIELD_MANAGER=veloxsearch-bootstrap
K3S_CONFIG=/etc/rancher/k3s/config.yaml
K3S_TOKEN_FILE=/etc/rancher/k3s/veloxsearch-token
K3S_SERVER_TOKEN=/var/lib/rancher/k3s/server/token
MARKER="install-veloxsearch-full.sh"
SYSCTL_FILE=/etc/sysctl.d/90-veloxsearch.conf
MODULES_FILE=/etc/modules-load.d/veloxsearch.conf
# Pod and service networks: k3s defaults, needed in the firewall's trust list.
POD_CIDR=10.42.0.0/16
SVC_CIDR=10.43.0.0/16

CMD="" PEERS="" SERVER="" TOKEN_FILE_IN="" TOKEN_STDIN=0 NODE_IP=""
K3S_VERSION="$K3S_VERSION_DEFAULT" VERSION="" MANIFEST="" LONGHORN=""
EXPECT_NODES=3 SKIP_FIREWALL=0 KEEP_SWAP=0 SKIP_PREREQS=0 SKIP_PLATFORM=0
SSH_USER="${SUDO_USER:-root}" SSH_KEY="" SSH_PORT=22 CONFIRM="" PURGE_DATA=0
SANS=()

usage() {
  cat <<EOF
Usage: sudo $0 <command> [options]

Commands:
  install     (on the FIRST node) prepare this node, create the cluster, join
              every --peers node over SSH, then install Longhorn + VeloxSearch
  init        (first node, manual path) prepare + start the first k3s server
  join        (nodes 2 and 3, manual path) prepare + join --server
  platform    (any server node, after all have joined) Longhorn + VeloxSearch
  preflight   check this node against the requirements; change nothing
  status      nodes, storage and VeloxSearch pods, as this node sees them
  uninstall   remove k3s from THIS node (destroys its data; asks first)

Cluster:
  --peers A,B            other nodes for 'install' (IP or SSH host name)
  --ssh-user USER        SSH user on the peers (default: ${SSH_USER});
                         needs passwordless sudo unless it is root
  --ssh-key FILE         SSH private key for the peers
  --ssh-port N           SSH port on the peers (default 22)
  --server URL           'join': a server's API, https://<node-1>:6443
  --token-file FILE      'join': file holding node 1's
                         ${K3S_SERVER_TOKEN} (it pins the cluster CA)
  --token-stdin          'join': read that token from standard input
  --node-ip IP           this node's cluster address (default: the source
                         address of the default route)
  --tls-san NAME         extra API name/IP (a VIP or DNS name in front of the
                         three nodes); repeatable
  --expect-nodes N       'platform' waits for N Ready nodes (default 3)
  --k3s-version V        k3s release (default ${K3S_VERSION_DEFAULT})

VeloxSearch:
  --version X.Y.Z        VeloxSearch release (default: the latest)
  --manifest PATH|URL    use this install.yaml instead of the release's
  --longhorn PATH|URL    use this Longhorn bundle instead of the release's
  --skip-platform        'install': stop after the cluster is up

Host:
  --skip-prereqs         do not install packages / touch sysctl, swap, firewall
  --skip-firewall        leave firewalld/ufw alone (open the ports yourself)
  --keep-swap            do not turn swap off (OpenSearch prefers it off)

Uninstall:
  --confirm HOSTNAME     answer the confirmation non-interactively
  --purge-data           also delete /var/lib/longhorn on this node

Environment: VELOX_REPO (owner/name, default ${REPO}), VELOX_K3S_TOKEN
(the 'join' token, instead of --token-file).
EOF
}

say()  { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
info() { printf '    %s\n' "$*"; }
warn() { printf '\033[1;33m!!\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31mxx\033[0m %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

[ $# -gt 0 ] || { usage; exit 1; }
case "$1" in
  install|init|join|platform|preflight|status|uninstall) CMD="$1"; shift ;;
  -h|--help|help) usage; exit 0 ;;
  *) usage >&2; die "unknown command: $1" ;;
esac

while [ $# -gt 0 ]; do
  case "$1" in
    --peers) PEERS="${2:?--peers needs A,B}"; shift 2 ;;
    --ssh-user) SSH_USER="${2:?--ssh-user needs a name}"; shift 2 ;;
    --ssh-key) SSH_KEY="${2:?--ssh-key needs a file}"; shift 2 ;;
    --ssh-port) SSH_PORT="${2:?--ssh-port needs a number}"; shift 2 ;;
    --server) SERVER="${2:?--server needs a URL}"; shift 2 ;;
    --token-file) TOKEN_FILE_IN="${2:?--token-file needs a file}"; shift 2 ;;
    --token-stdin) TOKEN_STDIN=1; shift ;;
    --node-ip) NODE_IP="${2:?--node-ip needs an address}"; shift 2 ;;
    --tls-san) SANS+=("${2:?--tls-san needs a name}"); shift 2 ;;
    --expect-nodes) EXPECT_NODES="${2:?--expect-nodes needs N}"; shift 2 ;;
    --k3s-version) K3S_VERSION="${2:?--k3s-version needs a version}"; shift 2 ;;
    --version) VERSION="${2:?--version needs X.Y.Z}"; VERSION="${VERSION#v}"; shift 2 ;;
    --manifest) MANIFEST="${2:?--manifest needs a path or URL}"; shift 2 ;;
    --longhorn) LONGHORN="${2:?--longhorn needs a path or URL}"; shift 2 ;;
    --skip-platform) SKIP_PLATFORM=1; shift ;;
    --skip-prereqs) SKIP_PREREQS=1; shift ;;
    --skip-firewall) SKIP_FIREWALL=1; shift ;;
    --keep-swap) KEEP_SWAP=1; shift ;;
    --confirm) CONFIRM="${2:?--confirm needs the hostname}"; shift 2 ;;
    --purge-data) PURGE_DATA=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown option: $1" ;;
  esac
done

# Values that end up in YAML or a remote shell: a closed character set, so
# nothing typed on the command line can inject a key or a command.
for s in ${SANS[@]+"${SANS[@]}"} ${NODE_IP:+"$NODE_IP"}; do
  [[ "$s" =~ ^[A-Za-z0-9.:-]+$ ]] || die "not a host name or address: $s"
done
[[ "$EXPECT_NODES" =~ ^[1-9][0-9]*$ ]] || die "--expect-nodes must be a positive number"
[[ "$SSH_PORT" =~ ^[0-9]+$ ]] || die "--ssh-port must be a number"
[[ "$K3S_VERSION" =~ ^v[0-9]+\.[0-9]+\.[0-9]+\+k3s[0-9]+$ ]] || die "--k3s-version looks like v1.36.4+k3s1"
[ -z "$VERSION" ] || [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "--version looks like 0.15.0"

[ "$(id -u)" -eq 0 ] || die "run as root (sudo $0 $CMD ...)"
[ "$(uname -s)" = Linux ] || die "Linux only"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

fetch() { # url dest
  if have curl; then curl -fsSL --retry 3 --proto '=https' -o "$2" "$1"
  elif have wget; then wget -q -O "$2" "$1"
  else die "need curl or wget"
  fi
}
fetch_or_copy() { # path-or-url dest
  case "$1" in
    https://*) fetch "$1" "$2" ;;
    *) [ -f "$1" ] || die "no such file: $1"; cp "$1" "$2" ;;
  esac
}

kc() { /usr/local/bin/k3s kubectl "$@"; }
k3s_ours() { [ -f "$K3S_CONFIG" ] && grep -q "$MARKER" "$K3S_CONFIG"; }
k3s_present() { [ -x /usr/local/bin/k3s ] || [ -f /etc/systemd/system/k3s.service ]; }
# uname -n, not hostname(1): minimal images (Amazon Linux, SLES) lack the latter.
node_name() { uname -n | tr '[:upper:]' '[:lower:]'; }

detect_node_ip() {
  [ -n "$NODE_IP" ] && return 0
  if have ip; then
    NODE_IP="$(ip -4 route get 1.1.1.1 2>/dev/null \
      | awk '{for (i = 1; i < NF; i++) if ($i == "src") { print $(i + 1); exit }}')"
  fi
  if [ -z "$NODE_IP" ]; then NODE_IP="$(hostname -I 2>/dev/null | awk '{print $1}')"; fi
  [ -n "$NODE_IP" ] || die "could not work out this node's address; pass --node-ip"
}

os_field() { sed -n "s/^$1=//p" /etc/os-release 2>/dev/null | tr -d '"' | head -n1; }
FAMILY=""
detect_family() {
  local id like
  id="$(os_field ID)"; like="$(os_field ID_LIKE)"
  case " $id $like " in
    *" debian "*|*" ubuntu "*) FAMILY=deb ;;
    *" rhel "*|*" fedora "*|*" centos "*|*" amzn "*) FAMILY=rpm ;;
    *" suse "*|*" sles "*|*" opensuse "*) FAMILY=suse ;;
    *) FAMILY="" ;;
  esac
}

# ---------------------------------------------------------------- preflight
# Hard failures stop before anything is touched; warnings are printed and the
# install goes on. The thresholds are docs/REQUIREMENTS.md's, per node: R4
# asks 8 GiB / 2 vCPU schedulable across the cluster, and a node under 4 GiB
# cannot host an OpenSearch pod next to k3s, etcd and Longhorn.
PREFLIGHT_DONE=0
preflight() {
  [ "$PREFLIGHT_DONE" -eq 0 ] || return 0
  local fails=0 mem_mib cpus free_gib p
  fail_() { printf '\033[1;31m  fail\033[0m %s\n' "$*" >&2; fails=$((fails + 1)); }
  ok_()   { printf '\033[1;32m    ok\033[0m %s\n' "$*"; }
  warn_() { printf '\033[1;33m  warn\033[0m %s\n' "$*" >&2; }

  say "preflight on $(node_name)"
  if [ -d /run/systemd/system ]; then ok_ "systemd"; else fail_ "systemd is not running (k3s installs as a systemd service)"; fi
  case "$(uname -m)" in
    x86_64) ok_ "amd64" ;;
    *) fail_ "$(uname -m): VeloxSearch v1 supports amd64 nodes only (R5)" ;;
  esac
  detect_family
  if [ -n "$FAMILY" ]; then ok_ "$(os_field PRETTY_NAME)"
  elif [ "$SKIP_PREREQS" -eq 1 ]; then warn_ "unrecognised distribution; --skip-prereqs, so its packages are yours"
  else fail_ "unrecognised distribution ($(os_field PRETTY_NAME)): install open-iscsi + an NFS client yourself and re-run with --skip-prereqs"
  fi

  mem_mib="$(awk '/^MemTotal:/ {print int($2 / 1024)}' /proc/meminfo)"
  if [ "$mem_mib" -lt 3800 ]; then fail_ "${mem_mib} MiB RAM: under 4 GiB per node"
  elif [ "$mem_mib" -lt 7600 ]; then warn_ "${mem_mib} MiB RAM: 8 GiB or more per node is recommended"
  else ok_ "${mem_mib} MiB RAM"
  fi
  cpus="$(nproc)"
  if [ "$cpus" -lt 2 ]; then fail_ "$cpus vCPU: at least 2 per node"; else ok_ "$cpus vCPU"; fi

  # Longhorn keeps replicas under /var/lib/longhorn and reserves 30% of the
  # disk for the OS (bootstrap/longhorn.yaml), k3s its images under
  # /var/lib/rancher: both live on /var/lib's filesystem.
  free_gib="$(df -Pk /var/lib | awk 'NR == 2 {print int($4 / 1024 / 1024)}')"
  if [ "$free_gib" -lt 30 ]; then fail_ "${free_gib} GiB free on /var/lib: at least 30 GiB"
  elif [ "$free_gib" -lt 100 ]; then warn_ "${free_gib} GiB free on /var/lib: data volumes come out of it; 100 GiB+ recommended"
  else ok_ "${free_gib} GiB free on /var/lib"
  fi

  if k3s_present; then
    if k3s_ours; then ok_ "k3s already installed by this script (re-run)"
    else fail_ "k3s is already installed here, not by this script: uninstall it, or join this node by hand"
    fi
  else
    if [ -d /etc/kubernetes/manifests ] || pgrep -x kubelet >/dev/null 2>&1; then
      fail_ "another Kubernetes (kubelet) runs on this node"
    fi
    if have ss; then
      for p in 80 443 2379 2380 6443 10250; do
        if [ -n "$(ss -Hltn "sport = :$p" 2>/dev/null)" ]; then fail_ "port $p is already in use"; fi
      done
    fi
  fi

  case "$(node_name)" in
    ""|localhost|localhost.*) fail_ "hostname is '$(node_name)': every node needs its own name (hostnamectl set-hostname)" ;;
    *) ok_ "hostname $(node_name)" ;;
  esac
  if [ -n "$(swapon --noheadings --show 2>/dev/null)" ]; then
    if [ "$KEEP_SWAP" -eq 1 ]; then warn_ "swap is on (--keep-swap)"; else info "swap is on: it will be turned off"; fi
  fi
  if have timedatectl && [ "$(timedatectl show -p NTPSynchronized --value 2>/dev/null)" != yes ]; then
    warn_ "clock not NTP-synchronised: etcd and TLS need the three clocks in step (chrony / systemd-timesyncd)"
  fi
  if have curl; then
    for p in https://get.k3s.io https://github.com https://registry-1.docker.io/v2/; do
      if curl -s -o /dev/null -m 20 --retry 2 "$p"; then ok_ "reaches $p"; else fail_ "cannot reach $p (VeloxSearch needs registry egress, R6)"; fi
    done
  else
    # A minimal image: prepare_node installs curl before anything downloads.
    warn_ "no curl yet (it gets installed): egress not checked"
  fi

  [ "$fails" -eq 0 ] || die "preflight: $fails problem(s) on $(node_name); nothing was changed"
  PREFLIGHT_DONE=1
}

# -------------------------------------------------------------- node prereqs
prepare_node() {
  if [ "$SKIP_PREREQS" -eq 1 ]; then say "prerequisites skipped (--skip-prereqs)"; return 0; fi
  detect_family
  say "packages: open-iscsi, NFS client, cryptsetup (Longhorn, R3)"
  # curl only when absent: RHEL 9 and Amazon Linux ship curl-minimal, which
  # conflicts with the full curl package and fails the whole transaction.
  local extra=()
  have curl || extra=(curl)
  case "$FAMILY" in
    deb)
      export DEBIAN_FRONTEND=noninteractive
      apt-get update -qq
      apt-get install -y -qq ${extra[@]+"${extra[@]}"} ca-certificates open-iscsi nfs-common cryptsetup util-linux >/dev/null ;;
    rpm)
      local pm=dnf; have dnf || pm=yum
      "$pm" install -y -q ${extra[@]+"${extra[@]}"} iscsi-initiator-utils nfs-utils cryptsetup util-linux >/dev/null ;;
    suse)
      zypper -n --gpg-auto-import-keys -q install ${extra[@]+"${extra[@]}"} open-iscsi nfs-client cryptsetup util-linux >/dev/null ;;
    *) die "unrecognised distribution; install the packages yourself and pass --skip-prereqs" ;;
  esac
  # RHEL ships an empty initiator name on some images; iscsid refuses to
  # start without one and Longhorn then cannot attach a single volume.
  if [ ! -s /etc/iscsi/initiatorname.iscsi ] && have iscsi-iname; then
    echo "InitiatorName=$(iscsi-iname)" > /etc/iscsi/initiatorname.iscsi
  fi
  systemctl enable --now iscsid >/dev/null 2>&1 || warn "iscsid did not start: Longhorn volumes will not attach"

  say "kernel modules and sysctls"
  install -d -m 0755 "${MODULES_FILE%/*}" "${SYSCTL_FILE%/*}"
  printf '# %s: Longhorn (iSCSI, encrypted volumes) and k3s networking\niscsi_tcp\ndm_crypt\noverlay\nbr_netfilter\n' \
    "$MARKER" > "$MODULES_FILE"
  local m
  for m in iscsi_tcp dm_crypt overlay br_netfilter; do
    modprobe "$m" 2>/dev/null || warn "kernel module $m did not load"
  done
  # vm.max_map_count: OpenSearch refuses to start below 262144. The inotify
  # limits: kubelet, k3s and the log shippers each hold watches per pod.
  cat > "$SYSCTL_FILE" <<EOF
# ${MARKER}: OpenSearch and Kubernetes limits
vm.max_map_count = 262144
fs.inotify.max_user_instances = 8192
fs.inotify.max_user_watches = 524288
EOF
  sysctl -q --system >/dev/null 2>&1 || true
  if [ "$(sysctl -n vm.max_map_count 2>/dev/null || echo 0)" -lt 262144 ]; then
    warn "vm.max_map_count is still below 262144 (a container host?): OpenSearch will not start on this node"
  fi

  if [ "$KEEP_SWAP" -eq 0 ] && [ -n "$(swapon --noheadings --show 2>/dev/null)" ]; then
    say "swap off (and commented out of /etc/fstab; a copy is at /etc/fstab.veloxsearch.bak)"
    swapoff -a || warn "swapoff failed: turn swap off yourself before OpenSearch runs here"
    if grep -Eq '^[^#].*[[:space:]]swap[[:space:]]' /etc/fstab; then
      cp -p /etc/fstab /etc/fstab.veloxsearch.bak
      sed -i -E 's|^([^#].*[[:space:]]swap[[:space:]].*)$|# \1  # disabled by '"$MARKER"'|' /etc/fstab
    fi
  fi

  # Longhorn KB "multipathd: volumes already mounted": multipathd claims the
  # iSCSI disks Longhorn attaches. Blacklisting sd* is Longhorn's own fix,
  # applied only when this host has no multipath device in use — on a host
  # whose own disks run over multipath it would cut them off.
  if systemctl is-active --quiet multipathd 2>/dev/null; then
    if grep -q "$MARKER" /etc/multipath.conf 2>/dev/null; then :
    elif [ -z "$(multipath -ll 2>/dev/null)" ]; then
      say "multipathd: blacklisting sd* devices (Longhorn KB)"
      printf '\n# %s: keep multipathd off Longhorn volumes\nblacklist {\n    devnode "^sd[a-z0-9]+"\n}\n' \
        "$MARKER" >> /etc/multipath.conf
      systemctl restart multipathd
    else
      warn "multipathd manages devices on this host: blacklist Longhorn's disks yourself (Longhorn KB, multipath)"
    fi
  fi

  # k3s docs: nm-cloud-setup rewrites routes under flannel and breaks pod
  # networking on RHEL-family cloud images.
  if systemctl is-enabled --quiet nm-cloud-setup.service 2>/dev/null; then
    systemctl disable --now nm-cloud-setup.service nm-cloud-setup.timer >/dev/null 2>&1 || true
    warn "nm-cloud-setup disabled as k3s requires: reboot this node once the install is done"
  fi

  configure_firewall
}

configure_firewall() {
  local p
  # API, etcd peers/clients, kubelet, flannel VXLAN, flannel WireGuard,
  # ingress (Traefik on every node), the console's NodePort.
  local tcp=(6443 2379-2380 10250 80 443 30080) udp=(8472 51820 51821)
  if [ "$SKIP_FIREWALL" -eq 1 ]; then
    say "firewall left alone (--skip-firewall): open tcp ${tcp[*]} and udp ${udp[*]} between the nodes"
    return 0
  fi
  if systemctl is-active --quiet firewalld 2>/dev/null; then
    say "firewalld: opening the k3s ports and trusting the pod/service networks"
    for p in "${tcp[@]}"; do firewall-cmd -q --permanent --add-port="$p/tcp"; done
    for p in "${udp[@]}"; do firewall-cmd -q --permanent --add-port="$p/udp"; done
    firewall-cmd -q --permanent --zone=trusted --add-source="$POD_CIDR"
    firewall-cmd -q --permanent --zone=trusted --add-source="$SVC_CIDR"
    firewall-cmd -q --reload
  elif have ufw && ufw status 2>/dev/null | grep -q '^Status: active'; then
    say "ufw: opening the k3s ports and allowing the pod/service networks"
    for p in "${tcp[@]}"; do ufw allow "${p/-/:}/tcp" >/dev/null; done
    for p in "${udp[@]}"; do ufw allow "$p/udp" >/dev/null; done
    ufw allow from "$POD_CIDR" >/dev/null
    ufw allow from "$SVC_CIDR" >/dev/null
  fi
}

# ---------------------------------------------------------------------- k3s
write_k3s_config() { # role: init | join
  local s
  install -d -m 0755 /etc/rancher/k3s
  {
    echo "# Written by ${MARKER} (VeloxSearch HA cluster); k3s reads it at start."
    echo "node-ip: \"${NODE_IP}\""
    if [ "$1" = init ]; then echo "cluster-init: true"; else echo "server: \"${SERVER}\""; fi
    echo "token-file: \"${K3S_TOKEN_FILE}\""
    echo "tls-san:"
    echo "  - \"${NODE_IP}\""
    for s in ${SANS[@]+"${SANS[@]}"}; do echo "  - \"${s}\""; done
    # Replicated storage only: see the header. Longhorn arrives in 'platform'.
    echo "disable:"
    echo "  - local-storage"
    echo "write-kubeconfig-mode: \"0600\""
    echo "secrets-encryption: true"
    # Every 6 h, two days kept, on each server (k3s defaults are 12 h / 5).
    echo "etcd-snapshot-schedule-cron: \"0 */6 * * *\""
    echo "etcd-snapshot-retention: 8"
  } > "$K3S_CONFIG"
  chmod 0600 "$K3S_CONFIG"
}

install_k3s() { # role
  if k3s_ours && systemctl is-active --quiet k3s; then
    say "k3s already running here (installed by this script): left as it is"
    return 0
  fi
  k3s_present && ! k3s_ours && die "k3s is already installed here, not by this script"
  write_k3s_config "$1"
  say "k3s ${K3S_VERSION} ($1) — this takes a minute or two"
  fetch https://get.k3s.io "$TMP/k3s-install.sh"
  INSTALL_K3S_VERSION="$K3S_VERSION" INSTALL_K3S_EXEC=server sh "$TMP/k3s-install.sh"
  wait_own_node
}

wait_own_node() {
  local n i
  n="$(node_name)"
  for i in $(seq 1 60); do
    kc get node "$n" >/dev/null 2>&1 && break
    [ "$i" -eq 60 ] && die "node $n never registered: journalctl -u k3s"
    sleep 5
  done
  kc wait --for=condition=Ready "node/$n" --timeout=300s >/dev/null \
    || die "node $n is not Ready: journalctl -u k3s"
  say "node $n Ready"
}

cmd_init() {
  detect_node_ip
  preflight
  prepare_node
  if [ ! -s "$K3S_TOKEN_FILE" ]; then
    install -d -m 0755 /etc/rancher/k3s
    (umask 077; od -An -N32 -tx1 /dev/urandom | tr -d ' \n' > "$K3S_TOKEN_FILE")
  fi
  install_k3s init
  join_hint
}

join_hint() {
  [ -n "$PEERS" ] && return 0
  cat <<EOF

    The first server is up. On each of the other two nodes, copy this script
    and the cluster token (a secret: it lets a machine join as a server):

      node 1:   sudo cat ${K3S_SERVER_TOKEN} > token     # then copy 'token' over
      node N:   sudo $0 join --server https://${NODE_IP}:6443 --token-file token

    When all three are Ready, back on this node:  sudo $0 platform
EOF
}

cmd_join() {
  [ -n "$SERVER" ] || die "join needs --server https://<node-1>:6443"
  [[ "$SERVER" =~ ^https://[A-Za-z0-9.:-]+:[0-9]+$ ]] || die "--server looks like https://10.0.0.11:6443"
  local token=""
  if [ "$TOKEN_STDIN" -eq 1 ]; then IFS= read -r token || true
  elif [ -n "$TOKEN_FILE_IN" ]; then token="$(head -n1 "$TOKEN_FILE_IN")"
  else token="${VELOX_K3S_TOKEN:-}"
  fi
  if ! k3s_ours; then
    [ -n "$token" ] || die "join needs the cluster token: --token-file, --token-stdin or VELOX_K3S_TOKEN"
    # The full K10<ca-hash>::server:<secret> form: the joiner checks node 1's
    # CA against the hash before trusting it with anything.
    [[ "$token" =~ ^K10[0-9a-f]+::server:[0-9a-f]+$ ]] \
      || die "that is not the server token from ${K3S_SERVER_TOKEN} on node 1"
  fi
  detect_node_ip
  preflight
  if [ -n "$token" ]; then
    install -d -m 0755 /etc/rancher/k3s
    (umask 077; printf '%s\n' "$token" > "$K3S_TOKEN_FILE")
  fi
  prepare_node
  install_k3s join
}

# ------------------------------------------------------ install (over SSH)
SSH_ARGS=()
# shellcheck disable=SC2029  # the command line is built for the peer on purpose
remote() { ssh "${SSH_ARGS[@]}" "${SSH_USER}@$1" "${@:2}"; }

cmd_install() {
  local self peers=() p sudo="" host names tmpf ip_arg
  self="$(readlink -f "$0")"
  if [ -n "$PEERS" ]; then
    [ -f "$self" ] || die "save the script to a file first: --peers copies it to the other nodes"
    IFS=, read -r -a peers <<< "$PEERS"
    for p in "${peers[@]}"; do [[ "$p" =~ ^[A-Za-z0-9.-]+$ ]] || die "not a host: $p"; done
    have ssh || die "--peers needs an SSH client"
    if [ $(( (${#peers[@]} + 1) % 2 )) -eq 0 ]; then
      warn "$(( ${#peers[@]} + 1 )) servers: etcd wants an odd number (3 tolerates one loss; 4 still only one)"
    fi
    EXPECT_NODES=$(( ${#peers[@]} + 1 ))
    SSH_ARGS=(-p "$SSH_PORT" -o BatchMode=yes -o StrictHostKeyChecking=accept-new -o ConnectTimeout=10)
    [ -z "$SSH_KEY" ] || SSH_ARGS+=(-i "$SSH_KEY")
    [ "$SSH_USER" = root ] || sudo="sudo -n"
  else
    warn "no --peers: a single-node cluster, no redundancy (--expect-nodes ${EXPECT_NODES} still applies to 'platform')"
  fi

  detect_node_ip
  preflight

  # Every peer passes preflight BEFORE node 1 changes anything: a cluster
  # that is half-created because node 3 has 2 GiB of RAM is worse than none.
  names="$(node_name)"
  local -A remote_file=()
  for p in ${peers[@]+"${peers[@]}"}; do
    say "peer $p: copying the script and running preflight"
    remote "$p" true || die "cannot SSH to ${SSH_USER}@$p (key-based, BatchMode)"
    # shellcheck disable=SC2016  # expanded on the peer, not here
    tmpf="$(remote "$p" 'umask 077; f=$(mktemp /tmp/veloxsearch-install.XXXXXX); cat > "$f"; echo "$f"' < "$self")"
    remote_file[$p]="$tmpf"
    ip_arg=""
    [[ "$p" =~ ^[0-9.]+$ ]] && ip_arg="--node-ip $p"
    # shellcheck disable=SC2086,SC2046  # $sudo, $ip_arg and passthrough are word lists on purpose
    remote "$p" $sudo bash "$tmpf" preflight $ip_arg $(passthrough) \
      || die "peer $p failed preflight; nothing was changed on any node"
    host="$(remote "$p" uname -n | tr '[:upper:]' '[:lower:]')"
    case " $names " in *" $host "*) die "two nodes are both called '$host': give each its own hostname" ;; esac
    names="$names $host"
  done

  cmd_init

  for p in ${peers[@]+"${peers[@]}"}; do
    say "peer $p: joining the cluster"
    ip_arg=""
    [[ "$p" =~ ^[0-9.]+$ ]] && ip_arg="--node-ip $p"
    # The token travels on stdin, never in an argument a `ps` on the peer
    # would show.
    # shellcheck disable=SC2086,SC2046
    remote "$p" $sudo bash "${remote_file[$p]}" join --server "https://${NODE_IP}:6443" \
        --token-stdin $ip_arg $(passthrough) < "$K3S_SERVER_TOKEN" \
      || die "peer $p did not join; the nodes already in the cluster stay as they are — fix it and re-run"
    remote "$p" rm -f "${remote_file[$p]}" || true
  done

  if [ "$SKIP_PLATFORM" -eq 1 ]; then
    say "cluster ready (--skip-platform): run '$0 platform' to install Longhorn and VeloxSearch"
    return 0
  fi
  cmd_platform
}

passthrough() { # the options a peer must share with node 1
  local s
  printf -- '--k3s-version %s ' "$K3S_VERSION"
  for s in ${SANS[@]+"${SANS[@]}"}; do printf -- '--tls-san %s ' "$s"; done
  [ "$SKIP_FIREWALL" -eq 0 ] || printf -- '--skip-firewall '
  [ "$KEEP_SWAP" -eq 0 ] || printf -- '--keep-swap '
  [ "$SKIP_PREREQS" -eq 0 ] || printf -- '--skip-prereqs '
}

# ------------------------------------------------------------------ platform
ready_nodes() { kc get nodes --no-headers 2>/dev/null | awk '$2 == "Ready" {n++} END {print n + 0}'; }

resolve_version() {
  [ -n "$VERSION" ] && return 0
  local url
  url="$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/${REPO}/releases/latest")" \
    || die "could not look up the latest release; pass --version"
  VERSION="${url##*/v}"
  [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "latest release is '${url##*/}', not vX.Y.Z; pass --version"
}

release_url() { echo "https://github.com/${REPO}/releases/download/v${VERSION}/$1"; }

install_longhorn() {
  if [ "$(kc get storageclass longhorn -o jsonpath='{.provisioner}' 2>/dev/null)" = driver.longhorn.io ]; then
    say "Longhorn already installed: left as it is"
    return 0
  fi
  say "Longhorn (the bundle VeloxSearch ${VERSION} vendors)"
  if [ -n "$LONGHORN" ]; then
    fetch_or_copy "$LONGHORN" "$TMP/longhorn.yaml"
  elif grep -q ' longhorn.yaml$' "$TMP/SHA256SUMS"; then
    fetch "$(release_url longhorn.yaml)" "$TMP/longhorn.yaml"
    (cd "$TMP" && sha256sum --quiet --ignore-missing -c SHA256SUMS) || die "longhorn.yaml does not match SHA256SUMS"
  else
    # Releases before the bundle became an asset: the file at the release's
    # own tag (v* tags cannot move, the tag ruleset forbids it).
    fetch "https://raw.githubusercontent.com/${REPO}/v${VERSION}/deploy/bootstrap/longhorn.yaml" "$TMP/longhorn.yaml"
  fi
  kc apply --server-side --force-conflicts --field-manager="$FIELD_MANAGER" -f "$TMP/longhorn.yaml" >/dev/null
  info "waiting for longhorn-manager on every node"
  kc -n longhorn-system rollout status daemonset/longhorn-manager --timeout=900s >/dev/null \
    || die "longhorn-manager is not running everywhere: kubectl -n longhorn-system get pods (open-iscsi? disk?)"
  kc -n longhorn-system rollout status deploy/longhorn-driver-deployer --timeout=600s >/dev/null \
    || die "longhorn-driver-deployer did not come up"
  local i
  for i in $(seq 1 60); do
    [ "$(kc get storageclass longhorn -o jsonpath='{.provisioner}' 2>/dev/null)" = driver.longhorn.io ] && break
    [ "$i" -eq 60 ] && die "the longhorn StorageClass never appeared: kubectl -n longhorn-system get pods"
    sleep 10
  done
  say "Longhorn ready: StorageClass 'longhorn', 3 replicas per volume, default"
}

install_veloxsearch() {
  say "VeloxSearch ${VERSION}"
  if [ -n "$MANIFEST" ]; then
    fetch_or_copy "$MANIFEST" "$TMP/install.yaml"
  else
    fetch "$(release_url install.yaml)" "$TMP/install.yaml"
    (cd "$TMP" && sha256sum --quiet --ignore-missing -c SHA256SUMS) || die "install.yaml does not match SHA256SUMS"
    info "install.yaml matches SHA256SUMS"
  fi
  local ref
  ref="$(grep -Eo "${IMAGE_REPO}[@:][^[:space:]\"]+" "$TMP/install.yaml" | head -n1)"
  if have cosign && [ -n "$ref" ]; then
    cosign verify "$ref" \
      --certificate-identity-regexp "^https://github\\.com/${REPO//./\\.}/" \
      --certificate-oidc-issuer https://token.actions.githubusercontent.com >/dev/null 2>&1 \
      || die "the image $ref does not carry this repository's cosign signature"
    info "image signature verified (cosign)"
  fi
  kc apply -f "$TMP/install.yaml" >/dev/null
  info "waiting for the console (first pull of the image can take a few minutes)"
  kc -n "$NS" rollout status deploy/veloxsearch --timeout=900s >/dev/null \
    || die "the console did not become ready: kubectl -n $NS get pods; kubectl -n $NS logs deploy/veloxsearch"
  # The Postgres pod waits for the Secret the app writes on its first boot
  # (install.yaml), so it settles a few seconds after the console.
  kc -n "$NS" rollout status statefulset/veloxsearch-postgres --timeout=600s >/dev/null \
    || warn "veloxsearch-postgres is not ready yet: kubectl -n $NS describe pod veloxsearch-postgres-0"
}

cmd_platform() {
  k3s_present || die "no k3s on this node: run 'init' (or 'install') first"
  [ -n "$NODE_IP" ] || detect_node_ip
  say "waiting for ${EXPECT_NODES} Ready node(s)"
  local i
  for i in $(seq 1 120); do
    [ "$(ready_nodes)" -ge "$EXPECT_NODES" ] && break
    [ "$i" -eq 120 ] && die "only $(ready_nodes) of ${EXPECT_NODES} nodes Ready after 10 minutes: kubectl get nodes"
    sleep 5
  done
  kc get nodes -o wide

  resolve_version
  if [ -z "$MANIFEST" ] || [ -z "$LONGHORN" ]; then
    fetch "$(release_url SHA256SUMS)" "$TMP/SHA256SUMS" || die "release v${VERSION} not found"
  fi
  install_longhorn
  install_veloxsearch
  print_access
}

print_access() {
  local ips ip code="" i
  ips="$(kc get nodes -o jsonpath='{range .items[*]}{.status.addresses[?(@.type=="InternalIP")].address}{" "}{end}')"
  for i in $(seq 1 36); do
    code="$(curl -s -o /dev/null -m 5 -w '%{http_code}' "http://${NODE_IP}/api/auth_state" || true)"
    [ "$code" = 200 ] && break
    sleep 5
  done
  [ "$code" = 200 ] || warn "http://${NODE_IP}/ does not answer yet (HTTP ${code:-none}); the NodePort :30080 is the fallback"

  echo
  say "VeloxSearch is installed"
  echo "    Open any node (Traefik answers on every one of them):"
  for ip in $ips; do
    echo "      http://${ip}/        http://velox.${ip}.sslip.io/        http://${ip}:30080/"
  done
  for ip in ${SANS[@]+"${SANS[@]}"}; do echo "      http://${ip}/   (point it at the nodes)"; done
  cat <<EOF

    Next:
      1. /setup in the browser: create the admin (the password is yours alone)
      2. the conformity screen installs cert-manager and the OpenSearch operator
      3. create a deployment: its data lands on Longhorn, 3 replicas

    Operating the cluster:
      kubeconfig      /etc/rancher/k3s/k3s.yaml on any node (k3s kubectl ...);
                      to use it elsewhere, change 'server:' to a node or VIP
      etcd snapshots  every 6 h on each node, /var/lib/rancher/k3s/server/db/snapshots
                      — copy them off the nodes (k3s etcd-snapshot save --s3 ...)
      redundancy      any one of the ${EXPECT_NODES} nodes can fail; the console
                      reschedules to a live node within ~5 minutes
      HTTPS           set VELOX_COOKIE_SECURE=1 in the veloxsearch-env ConfigMap
                      once a TLS ingress fronts the console
EOF
}

# -------------------------------------------------------------------- status
cmd_status() {
  k3s_present || die "no k3s on this node"
  say "nodes";       kc get nodes -o wide
  say "storage";     kc get storageclass
  say "VeloxSearch"; kc -n "$NS" get pods -o wide
  say "Longhorn";    kc -n longhorn-system get pods --no-headers 2>/dev/null \
                       | awk '{s[$3]++} END {for (k in s) printf "    %s %d\n", k, s[k]}'
}

# ----------------------------------------------------------------- uninstall
cmd_uninstall() {
  local n answer
  n="$(node_name)"
  k3s_present || die "no k3s on this node"
  cat >&2 <<EOF

  This removes k3s from ${n}: every container, the node's etcd member and
  its share of the cluster state. On a three-node cluster, first drain and
  delete this node from another one (kubectl drain ${n} --ignore-daemonsets
  --delete-emptydir-data; kubectl delete node ${n}) so Longhorn rebuilds its
  replicas elsewhere. $( [ "$PURGE_DATA" -eq 1 ] && echo "With --purge-data, /var/lib/longhorn (volume replicas) is deleted too." )

EOF
  if [ -n "$CONFIRM" ]; then answer="$CONFIRM"
  elif [ -r /dev/tty ]; then printf "  Type this node's name (%s) to go on: " "$n" >&2; read -r answer < /dev/tty
  else die "no terminal: pass --confirm $n"
  fi
  [ "$answer" = "$n" ] || die "not confirmed; nothing removed"
  if [ -x /usr/local/bin/k3s-uninstall.sh ]; then /usr/local/bin/k3s-uninstall.sh
  else die "/usr/local/bin/k3s-uninstall.sh is missing; remove k3s by hand"
  fi
  rm -f "$K3S_TOKEN_FILE"
  if [ "$PURGE_DATA" -eq 1 ]; then rm -rf /var/lib/longhorn; fi
  say "k3s removed from ${n}. Kept: packages, ${SYSCTL_FILE}, ${MODULES_FILE}"
}

case "$CMD" in
  install) cmd_install ;;
  init) cmd_init ;;
  join) cmd_join ;;
  platform) cmd_platform ;;
  preflight) detect_node_ip; preflight; say "preflight passed on $(node_name)" ;;
  status) cmd_status ;;
  uninstall) cmd_uninstall ;;
esac

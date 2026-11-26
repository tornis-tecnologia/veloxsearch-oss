#!/usr/bin/env bash
# Copyright (C) 2026 Tornis Desenvolvimento
# SPDX-License-Identifier: AGPL-3.0-only
#
# Velox Agent host installer (ADR-069 amendment 1).
#
# Lays out one install root and one service, and nothing the operator did not
# ask for:
#
#   /opt/velox-agent/bin/velox-agent    root:root 0755 — the agent cannot replace its own binary
#   /opt/velox-agent/etc/agent.env      root:velox-agent 0640 — the service's environment
#   /opt/velox-agent/state/             velox-agent 0700 — identity (0600), Fluent Bit config + offsets
#   /etc/systemd/system/velox-agent.service
#   /usr/local/bin/velox-agent -> /opt/velox-agent/bin/velox-agent
#
# The service runs as the `velox-agent` system user with CAP_DAC_READ_SEARCH:
# it can READ every log on the host, as Filebeat running as root can, and
# write nothing outside its state directory. `--run-as-root` is Filebeat's
# model verbatim, for whoever wants exactly that.
#
# Fluent Bit comes from the project's own package repository
# (packages.fluentbit.io) or the distribution — the same commands the
# console's install guide shows. VeloxSearch does not build or redistribute
# it; the upstream package's own fluent-bit.service is disabled, because the
# agent supervises the engine itself. The agent comes from this repository's
# GitHub release `velox-agent-v<version>`, checked against SHA256SUMS (and its
# cosign signature when cosign is installed).
#
# Re-running upgrades in place. Usage: install.sh --help
set -euo pipefail

REPO="${VELOX_AGENT_REPO:-tornis-tecnologia/veloxsearch-oss}"
ROOT=/opt/velox-agent
SVC=velox-agent
UNIT=/etc/systemd/system/velox-agent.service
TAG_PREFIX=velox-agent-v
FB_KEY_URL=https://packages.fluentbit.io/fluentbit.key
FB_DOCS=https://docs.fluentbit.io/manual/installation/downloads

MESH="" TOKEN="" INSECURE=0 VERSION="" BINARY=""
AS_ROOT=0 PRIV_PORTS=0 SKIP_FB=0 SKIP_SYSTEMD=0 REPLACE=0 REENROLL=0
UNINSTALL=0 PURGE=0

usage() {
  cat <<EOF
Usage: sudo $0 [options]

Install or upgrade:
  --mesh URL                 Velox Mesh URL to enrol with (with --token)
  --token TOKEN              enrollment token from the console (Add agent)
  --insecure                 do not verify the Mesh's TLS certificate
                             (an ingress serving its self-signed default)
  --version X.Y.Z            agent release to install (default: the newest)
  --binary PATH              install this velox-agent binary, no download
  --run-as-root              run the service as root (Filebeat's model)
  --allow-privileged-ports   let inputs bind ports < 1024 (syslog on 514)
  --skip-fluent-bit          do not install Fluent Bit (already managed)
  --replace                  stop an agent started by hand (an earlier
                             install) and move its identity here
  --reenroll                 enrol again although this host has an identity
                             (the Mesh then shows a second agent)
  --skip-systemd             write files only, start nothing (containers, CI)

Remove:
  --uninstall                remove the service, binary and environment file;
                             keep the state (the identity) and Fluent Bit
  --purge                    with --uninstall: also delete ${ROOT} and the
                             ${SVC} user — the identity is gone for good

Environment: VELOX_AGENT_REPO (owner/name, default ${REPO}).
EOF
}

say()  { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m!!\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31mxx\033[0m %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --mesh) MESH="${2:?--mesh needs a URL}"; shift 2 ;;
    --token) TOKEN="${2:?--token needs a value}"; shift 2 ;;
    --insecure) INSECURE=1; shift ;;
    --version) VERSION="${2:?--version needs X.Y.Z}"; VERSION="${VERSION#v}"; VERSION="${VERSION#"$TAG_PREFIX"}"; shift 2 ;;
    --binary) BINARY="${2:?--binary needs a path}"; shift 2 ;;
    --run-as-root) AS_ROOT=1; shift ;;
    --allow-privileged-ports) PRIV_PORTS=1; shift ;;
    --skip-fluent-bit) SKIP_FB=1; shift ;;
    --skip-systemd) SKIP_SYSTEMD=1; shift ;;
    --replace) REPLACE=1; shift ;;
    --reenroll) REENROLL=1; shift ;;
    --uninstall) UNINSTALL=1; shift ;;
    --purge) PURGE=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown option: $1" ;;
  esac
done

[ "$(id -u)" -eq 0 ] || die "run as root (sudo $0 ...)"
[ "$(uname -s)" = Linux ] || die "Linux only; see ${FB_DOCS} for other systems"
if { [ -n "$MESH" ] && [ -z "$TOKEN" ]; } || { [ -z "$MESH" ] && [ -n "$TOKEN" ]; }; then
  die "--mesh and --token go together"
fi
[ "$PURGE" -eq 0 ] || [ "$UNINSTALL" -eq 1 ] || die "--purge only goes with --uninstall"

have() { command -v "$1" >/dev/null 2>&1; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

fetch() { # url dest
  if have curl; then curl -fsSL --retry 3 --proto '=https' -o "$2" "$1"
  elif have wget; then wget -q -O "$2" "$1"
  else die "need curl or wget"
  fi
}

systemd_ok() { [ "$SKIP_SYSTEMD" -eq 0 ] && [ -d /run/systemd/system ]; }

# ---------------------------------------------------------------- uninstall
if [ "$UNINSTALL" -eq 1 ]; then
  if systemd_ok; then
    systemctl disable --now "$SVC" 2>/dev/null || true
  fi
  rm -f "$UNIT"
  if systemd_ok; then systemctl daemon-reload; fi
  if [ "$(readlink /usr/local/bin/velox-agent 2>/dev/null)" = "$ROOT/bin/velox-agent" ]; then
    rm -f /usr/local/bin/velox-agent
  fi
  rm -rf "${ROOT:?}/bin" "${ROOT:?}/etc"
  if [ "$PURGE" -eq 1 ]; then
    rm -rf "${ROOT:?}"
    if getent passwd "$SVC" >/dev/null; then userdel "$SVC" 2>/dev/null || true; fi
    if getent group "$SVC" >/dev/null; then groupdel "$SVC" 2>/dev/null || true; fi
    say "velox-agent removed, state and user included (the Mesh still lists this host until you remove it there)"
  else
    say "velox-agent removed; the identity stays in $ROOT/state (--uninstall --purge deletes it)"
  fi
  say "Fluent Bit was left installed: remove it with your package manager if nothing else uses it"
  exit 0
fi

# ------------------------------------------------------------------ the host
[ -r /etc/os-release ] || die "/etc/os-release not found: cannot tell the distribution"
os_field() { # KEY — sourced in a subshell so os-release cannot clobber ours
  # shellcheck disable=SC1091
  (. /etc/os-release && eval "printf '%s' \"\${$1:-}\"")
}
OS_ID="$(os_field ID)"
OS_LIKE="$(os_field ID_LIKE)"
OS_VER="$(os_field VERSION_ID)"
OS_UBUNTU_CODENAME="$(os_field UBUNTU_CODENAME)"
OS_CODENAME="$(os_field VERSION_CODENAME)"
OS_CODENAME="${OS_CODENAME:-$OS_UBUNTU_CODENAME}"
OS_MAJOR="${OS_VER%%.*}"

case "$(uname -m)" in
  x86_64|amd64) ARCH=x86_64 ;;
  aarch64|arm64) ARCH=aarch64 ;;
  *) die "unsupported architecture $(uname -m): the release ships x86_64 and aarch64" ;;
esac

family() {
  case " $OS_ID $OS_LIKE " in
    *" raspbian "*) echo raspbian ;;
    *" ubuntu "*) echo ubuntu ;;
    *" debian "*) echo debian ;;
    *" amzn "*) echo amazon ;;
    *" fedora "*) [ "$OS_ID" = fedora ] && echo fedora || echo rhel ;;
    *" rhel "*|*" centos "*|*" rocky "*|*" almalinux "*|*" ol "*) echo rhel ;;
    *) echo unknown ;;
  esac
}
FAMILY="$(family)"
say "host: ${OS_ID} ${OS_VER} (${FAMILY}), ${ARCH}"

pkg_install() {
  case "$FAMILY" in
    debian|ubuntu|raspbian) DEBIAN_FRONTEND=noninteractive apt-get install -y -q "$@" ;;
    *) if have dnf; then dnf install -y -q "$@"; else yum install -y -q "$@"; fi ;;
  esac
}

find_fluent_bit() {
  for p in /opt/fluent-bit/bin/fluent-bit /usr/bin/fluent-bit /usr/local/bin/fluent-bit; do
    [ -x "$p" ] && { echo "$p"; return 0; }
  done
  command -v fluent-bit 2>/dev/null || return 1
}

# The repository lines are the ones docs.fluentbit.io publishes per
# distribution, and the ones frontend/views_mesh.jsx shows for manual installs.
install_fluent_bit() {
  case "$FAMILY" in
    debian|ubuntu|raspbian)
      apt-get update -q
      pkg_install ca-certificates curl gnupg
      local flavour="$FAMILY" codename="$OS_CODENAME"
      # A derivative (Mint, Pop!_OS) has its own codename; its packages are
      # Ubuntu's, so is its repository line.
      if [ "$FAMILY" = ubuntu ] && [ -n "$OS_UBUNTU_CODENAME" ]; then codename="$OS_UBUNTU_CODENAME"; fi
      [ -n "$codename" ] || die "cannot read VERSION_CODENAME from /etc/os-release"
      install -d -m 0755 /usr/share/keyrings
      fetch "$FB_KEY_URL" "$TMP/fluentbit.key"
      gpg --batch --yes --dearmor -o /usr/share/keyrings/fluentbit-keyring.gpg "$TMP/fluentbit.key"
      echo "deb [signed-by=/usr/share/keyrings/fluentbit-keyring.gpg] https://packages.fluentbit.io/${flavour}/${codename} ${codename} main" \
        > /etc/apt/sources.list.d/fluent-bit.list
      apt-get update -q
      pkg_install fluent-bit
      ;;
    rhel|amazon)
      local flavour releasever
      case "$FAMILY:$OS_ID" in
        amazon:*) flavour=amazonlinux; releasever="$OS_MAJOR" ;;
        rhel:almalinux|rhel:rocky) flavour=almalinux; releasever="$OS_MAJOR" ;;
        *) flavour=centos; releasever="$OS_MAJOR" ;;
      esac
      cat > /etc/yum.repos.d/fluent-bit.repo <<EOF
[fluent-bit]
name=Fluent Bit
baseurl=https://packages.fluentbit.io/${flavour}/${releasever}/
gpgcheck=1
gpgkey=${FB_KEY_URL}
repo_gpgcheck=1
enabled=1
EOF
      pkg_install fluent-bit
      ;;
    fedora)
      # Fedora packages Fluent Bit itself; the project has no Fedora repo.
      pkg_install fluent-bit
      ;;
    *)
      die "no Fluent Bit procedure for '${OS_ID}': install it from ${FB_DOCS} and re-run with --skip-fluent-bit"
      ;;
  esac
}

if [ "$SKIP_FB" -eq 0 ]; then
  if FB="$(find_fluent_bit)"; then
    say "Fluent Bit already installed: $FB ($("$FB" --version 2>/dev/null | head -1))"
  else
    say "installing Fluent Bit from the upstream repository"
    install_fluent_bit
    FB="$(find_fluent_bit)" || die "Fluent Bit was installed but no fluent-bit binary was found"
  fi
  # The package ships and may start its own service on a sample config. The
  # agent runs the engine as its child; a second, unmanaged one would only
  # compete for the same files.
  if systemd_ok && systemctl list-unit-files fluent-bit.service >/dev/null 2>&1 \
     && systemctl is-enabled --quiet fluent-bit 2>/dev/null; then
    say "disabling the package's own fluent-bit.service (the agent supervises the engine)"
    systemctl disable --now fluent-bit || true
  fi
else
  FB="$(find_fluent_bit || true)"
  [ -n "$FB" ] || warn "--skip-fluent-bit and no fluent-bit found: the agent will refuse to run until it is installed"
fi

# ----------------------------------------------------------------- the agent
latest_version() {
  fetch "https://api.github.com/repos/${REPO}/releases?per_page=100" "$TMP/releases.json"
  # No jq on a bare host: the tag names are one field, and drafts are not
  # listed to anonymous callers. Prereleases carry a '-' in the version.
  grep -o "\"tag_name\": *\"${TAG_PREFIX}[0-9][0-9.]*\"" "$TMP/releases.json" \
    | sed "s/.*\"${TAG_PREFIX}\([^\"]*\)\"/\1/" | sort -V | tail -1
}

if [ -n "$BINARY" ]; then
  [ -x "$BINARY" ] || die "--binary $BINARY is not an executable file"
  cp "$BINARY" "$TMP/velox-agent"
  say "using the local binary $BINARY"
else
  [ -n "$VERSION" ] || VERSION="$(latest_version)"
  [ -n "$VERSION" ] || die "no ${TAG_PREFIX}* release found in ${REPO}; pass --version or --binary"
  base="https://github.com/${REPO}/releases/download/${TAG_PREFIX}${VERSION}"
  asset="velox-agent-${VERSION}-${ARCH}-unknown-linux-musl.tar.gz"
  say "downloading velox-agent ${VERSION} (${ARCH})"
  fetch "${base}/${asset}" "$TMP/${asset}"
  fetch "${base}/SHA256SUMS" "$TMP/SHA256SUMS"
  (cd "$TMP" && grep " ${asset}\$" SHA256SUMS | sha256sum -c --quiet -) \
    || die "checksum mismatch for ${asset}: refusing to install"
  if have cosign; then
    fetch "${base}/${asset}.bundle" "$TMP/${asset}.bundle"
    cosign verify-blob --bundle "$TMP/${asset}.bundle" \
      --certificate-identity-regexp "^https://github.com/${REPO}/\.github/workflows/agent-release\.yml@" \
      --certificate-oidc-issuer https://token.actions.githubusercontent.com \
      "$TMP/${asset}" >/dev/null 2>&1 || die "cosign signature does not verify for ${asset}: refusing to install"
    say "signature verified (cosign)"
  else
    warn "cosign not installed: checksum verified, signature not checked"
  fi
  tar -xzf "$TMP/${asset}" -C "$TMP" velox-agent
fi
"$TMP/velox-agent" version >/dev/null || die "the downloaded velox-agent does not run on this host"

# ------------------------------------------------------------ user + layout
# A minimal image (a cloud "minimal" AMI, a container) may lack the account
# tools or runuser; a real host has them, so this is a no-op there.
if ! have useradd || ! have runuser; then
  case "$FAMILY" in
    debian|ubuntu|raspbian) apt-get update -q; pkg_install passwd util-linux ;;
    rhel|amazon|fedora) pkg_install shadow-utils util-linux ;;
  esac
fi

if [ "$AS_ROOT" -eq 1 ]; then
  RUN_USER=root RUN_GROUP=root
else
  RUN_USER="$SVC" RUN_GROUP="$SVC"
  if ! getent passwd "$SVC" >/dev/null; then
    nologin="$(command -v nologin || echo /bin/false)"
    useradd --system --user-group --home-dir "$ROOT" --no-create-home --shell "$nologin" \
      --comment "Velox Agent" "$SVC"
    say "created system user $SVC"
  fi
  # Group read where the distribution grants it (Debian's adm owns
  # /var/log/syslog, auth.log; systemd-journal reads the journal). The
  # capability in the unit covers everything else (RHEL's root-only
  # /var/log/messages); the groups keep `velox-agent doctor` and ad-hoc runs
  # as the user meaningful.
  for g in adm systemd-journal; do
    if getent group "$g" >/dev/null; then usermod -aG "$g" "$SVC"; fi
  done
fi

install -d -m 0755 -o root -g root "$ROOT" "$ROOT/bin" "$ROOT/etc"
install -d -m 0700 -o "$RUN_USER" -g "$RUN_GROUP" "$ROOT/state"
install -m 0755 -o root -g root "$TMP/velox-agent" "$ROOT/bin/velox-agent.new"
mv -f "$ROOT/bin/velox-agent.new" "$ROOT/bin/velox-agent"
if [ -e /usr/local/bin/velox-agent ] && [ ! -L /usr/local/bin/velox-agent ]; then
  warn "replacing the hand-installed /usr/local/bin/velox-agent with a link to $ROOT/bin/velox-agent"
fi
ln -sfn "$ROOT/bin/velox-agent" /usr/local/bin/velox-agent

# The environment file is the operator's after the first write: an edit
# (a proxy, VELOX_AGENT_POLL) survives an upgrade.
if [ ! -f "$ROOT/etc/agent.env" ]; then
  {
    echo "# velox-agent service environment (written by install.sh; yours to edit)"
    echo "VELOX_AGENT_STATE=$ROOT/state"
    [ -n "${FB:-}" ] && echo "VELOX_FLUENT_BIT=$FB"
    echo "# HTTPS_PROXY=http://proxy.example:3128"
  } > "$ROOT/etc/agent.env"
fi
chown root:"$RUN_GROUP" "$ROOT/etc/agent.env"
chmod 0640 "$ROOT/etc/agent.env"

# --------------------------------------------- an identity from earlier runs
# Earlier releases kept the state under the home of whoever ran `enroll`.
running_by_hand() {
  have pgrep || return 0
  pgrep -f "velox-agent run" | grep -vx "$$" || true
}

legacy_dirs() {
  echo /root/.local/state/velox-agent
  if [ -n "${SUDO_USER:-}" ] && [ "$SUDO_USER" != root ]; then
    echo "$(getent passwd "$SUDO_USER" | cut -d: -f6)/.local/state/velox-agent"
  fi
  echo /var/lib/velox-agent
}

if [ ! -f "$ROOT/state/identity.json" ]; then
  while read -r old; do
    if [ -z "$old" ] || [ ! -f "$old/identity.json" ]; then continue; fi
    pids="$(running_by_hand)"
    if [ -n "$pids" ] && [ "$REPLACE" -eq 0 ]; then
      warn "an identity is in $old and a velox-agent is running by hand (pid ${pids//$'\n'/ })"
      warn "left alone: re-run with --replace to stop it and move its identity here"
      break
    fi
    if [ -n "$pids" ]; then
      say "stopping the hand-started agent (pid ${pids//$'\n'/ }) and its engine"
      # shellcheck disable=SC2086
      kill $pids 2>/dev/null || true
      # The engine is the agent's child but outlives a signalled parent:
      # find it by its configuration path, as the agent's own reaper does.
      pkill -f "$old/fluent-bit/fluent-bit\." 2>/dev/null || true
      sleep 2
    fi
    say "moving the identity and engine state from $old"
    cp -a "$old/." "$ROOT/state/"
    rm -f "$ROOT/state/engine.pid" "$ROOT/state/fluent-bit/engine.pid" "$ROOT/state/identity.tmp"
    chown -R "$RUN_USER:$RUN_GROUP" "$ROOT/state"
    # cp -a carries the old directory's mode onto state/: put the layout's back.
    chmod 0700 "$ROOT/state"
    chmod 0600 "$ROOT/state/identity.json"
    mv "$old" "${old}.migrated-$(date +%Y%m%d%H%M%S)"
    break
  done < <(legacy_dirs)
fi

# ---------------------------------------------------------------- the unit
install -d -m 0755 "$(dirname "$UNIT")"   # absent where systemd never ran (a container)
CAPS="CAP_DAC_READ_SEARCH"
[ "$PRIV_PORTS" -eq 1 ] && CAPS="$CAPS CAP_NET_BIND_SERVICE"
{
  cat <<EOF
# Written by velox-agent install.sh — re-run it rather than editing here.
[Unit]
Description=Velox Agent (Velox Mesh fleet agent; supervises Fluent Bit)
Documentation=https://github.com/${REPO}/tree/main/veloxagent
Wants=network-online.target
After=network-online.target

[Service]
Type=simple
User=${RUN_USER}
Group=${RUN_GROUP}
EnvironmentFile=${ROOT}/etc/agent.env
ExecStart=${ROOT}/bin/velox-agent run
Restart=on-failure
RestartSec=10s
UMask=0077
EOF
  if [ "$AS_ROOT" -eq 0 ]; then
    cat <<EOF
# Read any file (logs), write none: DAC_READ_SEARCH bypasses read and search
# permission checks only. Inherited by the Fluent Bit child (ambient).
AmbientCapabilities=${CAPS}
CapabilityBoundingSet=${CAPS}
EOF
  fi
  cat <<EOF
NoNewPrivileges=yes
ProtectSystem=strict
ReadWritePaths=${ROOT}/state
ProtectHome=read-only
PrivateTmp=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
RestrictSUIDSGID=yes
LockPersonality=yes

[Install]
WantedBy=multi-user.target
EOF
} > "$UNIT.new"
chmod 0644 "$UNIT.new"
mv -f "$UNIT.new" "$UNIT"

as_service_user() {
  if [ "$RUN_USER" = root ]; then
    env VELOX_AGENT_STATE="$ROOT/state" ${FB:+VELOX_FLUENT_BIT="$FB"} "$@"
  else
    runuser -u "$RUN_USER" -- env VELOX_AGENT_STATE="$ROOT/state" ${FB:+VELOX_FLUENT_BIT="$FB"} "$@"
  fi
}

# ------------------------------------------------------------------ enrol
if [ -n "$TOKEN" ]; then
  if [ -f "$ROOT/state/identity.json" ] && [ "$REENROLL" -eq 0 ]; then
    warn "already enrolled (${ROOT}/state/identity.json): token not used — --reenroll enrols again"
  else
    previous=""
    if [ -f "$ROOT/state/identity.json" ]; then
      previous="$ROOT/state/identity.json.replaced-$(date +%Y%m%d%H%M%S)"
      mv "$ROOT/state/identity.json" "$previous"
    fi
    set -- enroll --mesh "$MESH" --token "$TOKEN"
    [ "$INSECURE" -eq 1 ] && set -- "$@" --insecure
    if ! as_service_user "$ROOT/bin/velox-agent" "$@"; then
      # A refused token must not cost the host the identity it had.
      if [ -n "$previous" ]; then mv "$previous" "$ROOT/state/identity.json"; fi
      die "enrollment failed (see above)"
    fi
  fi
fi

# ------------------------------------------------------------------ start
if systemd_ok; then
  systemctl daemon-reload
  systemctl enable "$SVC" >/dev/null 2>&1
  if [ -f "$ROOT/state/identity.json" ]; then
    systemctl restart "$SVC"
    say "velox-agent service (re)started"
  else
    warn "not enrolled: the service is enabled but not started"
    warn "enrol with: sudo $0 --mesh URL --token TOKEN   (or: sudo -u $RUN_USER VELOX_AGENT_STATE=$ROOT/state velox-agent enroll ...)"
  fi
else
  warn "systemd not used: start it yourself with  $ROOT/bin/velox-agent run  (as $RUN_USER, env from $ROOT/etc/agent.env)"
fi

say "doctor:"
as_service_user "$ROOT/bin/velox-agent" doctor | sed 's/^/    /' || warn "doctor reports a problem (above)"
if systemd_ok; then
  echo "    logs: journalctl -u $SVC -f    engine output: $ROOT/state/fluent-bit/"
fi

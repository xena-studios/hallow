#!/usr/bin/env bash
# Turns a blank Debian 12/13 or Ubuntu 24.04 machine into a Hallow node:
# Docker, Calagopus Wings, WireGuard, a locked firewall and kernel tuning,
# then registers the node in the panel with a one-time token. Safe to run
# again: every step checks what is already there. See deploy/node/README.md.
#
#   install.sh --control-plane https://cp.example.net --join-token hjt_... \
#     --role burst --region fsn1 --wg-address 10.42.1.7/16 \
#     --wg-peer 'PUBKEY|hub.example.net:51820|10.42.0.0/16|25'
#
# Every flag has a HALLOW_* environment variable; --env-file reads them from
# a KEY=VALUE file (what cloud-init writes). --dry-run prints every action.
set -euo pipefail

# Wings, pinned. The checksums are GitHub's published SHA-256 digests of the
# release assets. Point HALLOW_WINGS_URL at our own build to use it instead.
WINGS_VERSION_PINNED=1.2.10
WINGS_SHA256_X86_64=8efc06b95acffe9ea984375a727c9d67ea99647330d0363f2975b1485fc0d87d
WINGS_SHA256_AARCH64=8632c0f08a5780eeb9e71f54d6ab90cc91a731af4c8a029a5b16d091c4287470

usage() {
	cat <<'EOF'
Usage: install.sh [options]

Registration (one of):
  --control-plane URL     Control plane that exchanges the join token   HALLOW_CONTROL_PLANE_URL
  --join-token TOKEN      One-time join token from POST /v1/nodes/join-tokens  HALLOW_JOIN_TOKEN
  --panel-url URL         Panel URL, with --enroll-code                 HALLOW_PANEL_URL
  --enroll-code CODE      One-time Wings enrollment code from the panel HALLOW_ENROLL_CODE
  --skip-register         Install everything but don't register        HALLOW_SKIP_REGISTER=1
  --rejoin                Register again even if Wings is configured   HALLOW_REJOIN=1

Node:
  --role base|burst       Node kind (default burst)                     HALLOW_ROLE
  --region NAME           Region, such as fsn1                          HALLOW_REGION
  --label KEY=VALUE       Extra label; repeatable                       HALLOW_LABELS (space-separated)
  --private-ip IP         Address the panel and load balancer reach this node on
                          (default: the WireGuard address)              HALLOW_PRIVATE_IP

Private network:
  --wg-address CIDR       This node's WireGuard address, e.g. 10.42.1.7/16  HALLOW_WG_ADDRESS
  --wg-port PORT          WireGuard listen port (default 51820)         HALLOW_WG_PORT
  --wg-peer SPEC          PUBKEY|ENDPOINT|CIDR[,CIDR]|KEEPALIVE; repeatable  HALLOW_WG_PEERS (space-separated)
  --private-if NAME       Extra trusted private interface (vSwitch, vRack); repeatable  HALLOW_PRIVATE_IFS

Firewall:
  --ssh-allow CIDR        Allow SSH only from here; repeatable (default anywhere)  HALLOW_SSH_ALLOW
  --ssh-port PORT         SSH port (default 22)                         HALLOW_SSH_PORT
  --wings-allow CIDR      Also allow Wings API/SFTP from here; repeatable  HALLOW_WINGS_ALLOW
  --game-ports LO-HI      Panel allocation range (default 30000-30999)  HALLOW_GAME_PORTS
  --no-firewall           Leave the firewall alone                      HALLOW_NO_FIREWALL=1

Wings:
  --wings-version VER     Release to install (default 1.2.10)           HALLOW_WINGS_VERSION
  --wings-url URL         Download URL; {version} and {arch} are filled in  HALLOW_WINGS_URL
  --wings-sha256 HEX      Expected SHA-256 (required for a non-pinned build)  HALLOW_WINGS_SHA256

Other:
  --env-file PATH         Read HALLOW_* settings from a KEY=VALUE file
  --dry-run               Print what would be done; change nothing      HALLOW_DRY_RUN=1
  -h, --help
EOF
}

die() { printf 'install.sh: error: %s\n' "$*" >&2; exit 1; }
log() { printf '==> %s\n' "$*" >&2; }
# Indents what a dry run would write or send.
show() { local line; while IFS= read -r line; do printf '|   %s\n' "$line"; done; }

# Settings from a KEY=VALUE file, without evaluating it: only HALLOW_* keys,
# values taken literally. Variables already in the environment win.
read_env_file() {
	local file=$1 key value
	[[ -r $file ]] || die "can't read $file"
	while IFS= read -r line || [[ -n $line ]]; do
		[[ $line =~ ^[[:space:]]*(#|$) ]] && continue
		key=${line%%=*}
		value=${line#*=}
		[[ $key =~ ^HALLOW_[A-Z0-9_]+$ ]] || die "$file: unexpected line: $key"
		[[ -n ${!key+set} ]] || printf -v "$key" '%s' "$value"
	done <"$file"
}

# --env-file is read before anything else, so flags override it.
args=("$@")
for ((i = 0; i < ${#args[@]}; i++)); do
	if [[ ${args[i]} == --env-file ]]; then
		read_env_file "${args[i + 1]:-}"
	fi
done

CONTROL_PLANE_URL=${HALLOW_CONTROL_PLANE_URL:-}
JOIN_TOKEN=${HALLOW_JOIN_TOKEN:-}
PANEL_URL=${HALLOW_PANEL_URL:-}
ENROLL_CODE=${HALLOW_ENROLL_CODE:-}
SKIP_REGISTER=${HALLOW_SKIP_REGISTER:-0}
REJOIN=${HALLOW_REJOIN:-0}
ROLE=${HALLOW_ROLE:-burst}
REGION=${HALLOW_REGION:-}
read -r -a LABELS <<<"${HALLOW_LABELS:-}"
PRIVATE_IP=${HALLOW_PRIVATE_IP:-}
WG_ADDRESS=${HALLOW_WG_ADDRESS:-}
WG_PORT=${HALLOW_WG_PORT:-51820}
read -r -a WG_PEERS <<<"${HALLOW_WG_PEERS:-}"
read -r -a PRIVATE_IFS <<<"${HALLOW_PRIVATE_IFS:-}"
read -r -a SSH_ALLOW <<<"${HALLOW_SSH_ALLOW:-}"
SSH_PORT=${HALLOW_SSH_PORT:-22}
read -r -a WINGS_ALLOW <<<"${HALLOW_WINGS_ALLOW:-}"
GAME_PORTS=${HALLOW_GAME_PORTS:-30000-30999}
NO_FIREWALL=${HALLOW_NO_FIREWALL:-0}
WINGS_VERSION=${HALLOW_WINGS_VERSION:-$WINGS_VERSION_PINNED}
WINGS_URL_DEFAULT='https://github.com/calagopus/wings/releases/download/release-{version}/wings-rs-{arch}-linux'
WINGS_URL=${HALLOW_WINGS_URL:-$WINGS_URL_DEFAULT}
WINGS_SHA256=${HALLOW_WINGS_SHA256:-}
DRY_RUN=${HALLOW_DRY_RUN:-0}
OS_RELEASE=${HALLOW_OS_RELEASE:-/etc/os-release}
ENV_FILE=

while (($#)); do
	case $1 in
	--control-plane) CONTROL_PLANE_URL=${2:?}; shift ;;
	--join-token) JOIN_TOKEN=${2:?}; shift ;;
	--panel-url) PANEL_URL=${2:?}; shift ;;
	--enroll-code) ENROLL_CODE=${2:?}; shift ;;
	--skip-register) SKIP_REGISTER=1 ;;
	--rejoin) REJOIN=1 ;;
	--role) ROLE=${2:?}; shift ;;
	--region) REGION=${2:?}; shift ;;
	--label) LABELS+=("${2:?}"); shift ;;
	--private-ip) PRIVATE_IP=${2:?}; shift ;;
	--wg-address) WG_ADDRESS=${2:?}; shift ;;
	--wg-port) WG_PORT=${2:?}; shift ;;
	--wg-peer) WG_PEERS+=("${2:?}"); shift ;;
	--private-if) PRIVATE_IFS+=("${2:?}"); shift ;;
	--ssh-allow) SSH_ALLOW+=("${2:?}"); shift ;;
	--ssh-port) SSH_PORT=${2:?}; shift ;;
	--wings-allow) WINGS_ALLOW+=("${2:?}"); shift ;;
	--game-ports) GAME_PORTS=${2:?}; shift ;;
	--no-firewall) NO_FIREWALL=1 ;;
	--wings-version) WINGS_VERSION=${2:?}; shift ;;
	--wings-url) WINGS_URL=${2:?}; shift ;;
	--wings-sha256) WINGS_SHA256=${2:?}; shift ;;
	--env-file) ENV_FILE=${2:?}; shift ;;
	--dry-run) DRY_RUN=1 ;;
	-h | --help) usage; exit 0 ;;
	*) usage >&2; die "unknown option $1" ;;
	esac
	shift
done

# Checks

[[ $ROLE == base || $ROLE == burst ]] || die "--role must be base or burst, not $ROLE"
[[ $GAME_PORTS =~ ^([0-9]+)-([0-9]+)$ ]] || die "--game-ports must be LO-HI, not $GAME_PORTS"
GAME_LO=${BASH_REMATCH[1]} GAME_HI=${BASH_REMATCH[2]}
# Outgoing connections use 40000 and up (see the sysctl step), so they never
# take a port a game server is about to bind.
((GAME_LO >= 1024 && GAME_LO <= GAME_HI && GAME_HI < 40000)) || die "--game-ports must be within 1024-39999, not $GAME_PORTS"
[[ $SSH_PORT =~ ^[0-9]+$ ]] || die "--ssh-port must be a number"
[[ $WG_PORT =~ ^[0-9]+$ ]] || die "--wg-port must be a number"
[[ -z $REGION || $REGION =~ ^[A-Za-z0-9._-]{1,64}$ ]] || die "--region must be 1 to 64 letters, digits, '.', '_' or '-'"
for label in "${LABELS[@]}"; do
	[[ $label =~ ^[A-Za-z0-9._/-]{1,63}=[A-Za-z0-9._-]{0,63}$ ]] || die "bad label $label (KEY=VALUE)"
done
if [[ -n $WG_ADDRESS ]]; then
	[[ $WG_ADDRESS =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}/[0-9]{1,2}$ ]] || die "--wg-address must be IPv4/prefix, not $WG_ADDRESS"
	((${#WG_PEERS[@]})) || die "--wg-address needs at least one --wg-peer"
fi
for peer in "${WG_PEERS[@]}"; do
	[[ $peer =~ ^[A-Za-z0-9+/]{42,43}=?\|[^|]*\|[0-9a-fA-F.:/,]+(\|[0-9]*)?$ ]] ||
		die "bad --wg-peer $peer (PUBKEY|ENDPOINT|CIDR[,CIDR]|KEEPALIVE)"
done
for name in "${PRIVATE_IFS[@]}"; do
	[[ $name =~ ^[A-Za-z0-9._-]{1,15}$ ]] || die "bad interface name $name"
done
for cidr in "${SSH_ALLOW[@]}" "${WINGS_ALLOW[@]}"; do
	[[ $cidr =~ ^[0-9a-fA-F.:]+(/[0-9]{1,3})?$ ]] || die "bad CIDR $cidr"
done
[[ -z $PRIVATE_IP ]] && PRIVATE_IP=${WG_ADDRESS%/*}

REGISTER='none'
if [[ $SKIP_REGISTER == 1 ]]; then
	REGISTER='skip'
elif [[ -n $JOIN_TOKEN ]]; then
	[[ -n $CONTROL_PLANE_URL ]] || die "--join-token needs --control-plane"
	[[ -n $PRIVATE_IP ]] || die "joining needs --wg-address or --private-ip"
	REGISTER='join'
elif [[ -n $ENROLL_CODE ]]; then
	[[ -n $PANEL_URL ]] || die "--enroll-code needs --panel-url"
	REGISTER='enroll'
else
	die "give --join-token and --control-plane, --enroll-code and --panel-url, or --skip-register"
fi

# OS: Debian 12/13 or Ubuntu 24.04, nothing else.
[[ -r $OS_RELEASE ]] || die "can't read $OS_RELEASE; only Debian 12/13 and Ubuntu 24.04 are supported"
os_field() { sed -n "s/^$1=//p" "$OS_RELEASE" | tr -d "\"'" | head -n 1; }
OS_ID=$(os_field ID)
OS_VERSION=$(os_field VERSION_ID)
OS_CODENAME=$(os_field VERSION_CODENAME)
case "$OS_ID:$OS_VERSION" in
debian:12 | debian:13 | ubuntu:24.04) ;;
*) die "unsupported OS ${OS_ID:-unknown} ${OS_VERSION:-}; only Debian 12/13 and Ubuntu 24.04 are supported" ;;
esac
[[ -n $OS_CODENAME ]] || die "$OS_RELEASE has no VERSION_CODENAME"

case $(uname -m) in
x86_64 | amd64) ARCH=x86_64 PINNED_SHA=$WINGS_SHA256_X86_64 ;;
aarch64 | arm64) ARCH=aarch64 PINNED_SHA=$WINGS_SHA256_AARCH64 ;;
*) die "unsupported architecture $(uname -m)" ;;
esac
if [[ -z $WINGS_SHA256 ]]; then
	[[ $WINGS_VERSION == "$WINGS_VERSION_PINNED" && -z ${HALLOW_WINGS_URL:-} ]] ||
		die "a Wings build other than the pinned $WINGS_VERSION_PINNED needs --wings-sha256"
	WINGS_SHA256=$PINNED_SHA
fi
[[ $WINGS_SHA256 =~ ^[0-9a-f]{64}$ ]] || die "--wings-sha256 must be 64 lowercase hex digits"
WINGS_URL=${WINGS_URL//\{version\}/$WINGS_VERSION}
WINGS_URL=${WINGS_URL//\{arch\}/$ARCH}

if [[ $DRY_RUN != 1 && $(id -u) != 0 ]]; then
	die "run as root (or with --dry-run)"
fi

# Actions

run() {
	if [[ $DRY_RUN == 1 ]]; then
		printf '+ %s\n' "$*"
	else
		"$@"
	fi
}

# write_file PATH MODE < content. Rewrites only when the content changed;
# returns 0 if it did, 1 if it was already up to date.
write_file() {
	local path=$1 mode=$2 content
	content=$(cat)
	if [[ -f $path ]] && [[ $(cat "$path") == "$content" ]]; then
		return 1
	fi
	if [[ $DRY_RUN == 1 ]]; then
		printf '+ write %s (mode %s)\n' "$path" "$mode"
		printf '%s\n' "$content" | sed 's/^PrivateKey = .*/PrivateKey = <redacted>/' | show
		return 0
	fi
	install -d -m 0755 "$(dirname "$path")"
	local tmp
	tmp=$(mktemp "$path.XXXXXX")
	printf '%s\n' "$content" >"$tmp"
	chmod "$mode" "$tmp"
	mv -f "$tmp" "$path"
}

have_package() { dpkg-query -W -f='${Status}' "$1" 2>/dev/null | grep -q 'install ok installed'; }

install_packages() {
	local missing=() pkg
	for pkg in "$@"; do
		have_package "$pkg" || missing+=("$pkg")
	done
	((${#missing[@]})) || return 0
	run env DEBIAN_FRONTEND=noninteractive apt-get install -y -q --no-install-recommends "${missing[@]}"
}

step_packages() {
	log "base packages"
	run env DEBIAN_FRONTEND=noninteractive apt-get update -q
	install_packages ca-certificates curl gnupg jq nftables wireguard-tools iproute2
}

step_docker() {
	log "Docker from download.docker.com"
	local keyring=/etc/apt/keyrings/docker.asc
	if [[ ! -s $keyring ]]; then
		run install -d -m 0755 /etc/apt/keyrings
		run curl -fsSL --retry 5 "https://download.docker.com/linux/$OS_ID/gpg" -o "$keyring"
		run chmod a+r "$keyring"
	fi
	local dpkg_arch=amd64
	[[ $ARCH == aarch64 ]] && dpkg_arch=arm64
	if write_file /etc/apt/sources.list.d/docker.list 0644 <<EOF; then
deb [arch=$dpkg_arch signed-by=$keyring] https://download.docker.com/linux/$OS_ID $OS_CODENAME stable
EOF
		run env DEBIAN_FRONTEND=noninteractive apt-get update -q
	fi
	install_packages docker-ce docker-ce-cli containerd.io
	# Local log driver with rotation: game servers log a lot.
	if write_file /etc/docker/daemon.json 0644 <<'EOF'; then
{
  "log-driver": "local",
  "log-opts": { "max-size": "20m", "max-file": "3" },
  "live-restore": true
}
EOF
		run systemctl restart docker
	fi
	run systemctl enable --now docker
}

step_sysctl() {
	log "kernel tuning"
	write_file /etc/modules-load.d/hallow.conf 0644 <<'EOF' || true
tcp_bbr
EOF
	# Many connections per proxy, many containers per node, low latency.
	if write_file /etc/sysctl.d/90-hallow.conf 0644 <<'EOF'; then
net.core.default_qdisc = fq
net.ipv4.tcp_congestion_control = bbr
net.core.somaxconn = 8192
net.core.netdev_max_backlog = 16384
net.ipv4.tcp_max_syn_backlog = 8192
net.core.rmem_max = 16777216
net.core.wmem_max = 16777216
net.ipv4.tcp_rmem = 4096 87380 16777216
net.ipv4.tcp_wmem = 4096 65536 16777216
net.ipv4.tcp_slow_start_after_idle = 0
net.ipv4.tcp_mtu_probing = 1
net.ipv4.tcp_fin_timeout = 15
net.ipv4.ip_local_port_range = 40000 65535
net.ipv4.ip_forward = 1
net.netfilter.nf_conntrack_max = 1048576
fs.file-max = 2097152
fs.inotify.max_user_watches = 1048576
fs.inotify.max_user_instances = 8192
vm.max_map_count = 262144
vm.swappiness = 10
EOF
		run modprobe tcp_bbr
		run modprobe nf_conntrack
		run sysctl --system -q
	fi
}

step_wireguard() {
	[[ -n $WG_ADDRESS ]] || { log "WireGuard: no --wg-address, skipped"; return 0; }
	log "WireGuard wg0 at $WG_ADDRESS"
	local key=/etc/wireguard/wg0.key private_key="<generated on the node>"
	if [[ ! -s $key ]]; then
		if [[ $DRY_RUN == 1 ]]; then
			printf '+ wg genkey > %s (mode 0600)\n' "$key"
		else
			install -d -m 0700 /etc/wireguard
			(umask 077 && wg genkey >"$key")
		fi
	fi
	[[ -s $key ]] && private_key=$(cat "$key")
	local conf peer pubkey endpoint allowed keepalive
	conf="[Interface]
Address = $WG_ADDRESS
ListenPort = $WG_PORT
PrivateKey = $private_key"
	for peer in "${WG_PEERS[@]}"; do
		IFS='|' read -r pubkey endpoint allowed keepalive <<<"$peer"
		conf+="

[Peer]
PublicKey = $pubkey
AllowedIPs = $allowed"
		[[ -n $endpoint ]] && conf+="
Endpoint = $endpoint"
		[[ -n $keepalive ]] && conf+="
PersistentKeepalive = $keepalive"
	done
	if write_file /etc/wireguard/wg0.conf 0600 <<<"$conf"; then
		run systemctl enable wg-quick@wg0
		run systemctl restart wg-quick@wg0
	else
		run systemctl enable --now wg-quick@wg0
	fi
}

# nft set elements, comma-separated and quoted for interface names.
nft_list() {
	local quote=$1 out="" item
	shift
	for item in "$@"; do
		[[ $quote == 1 ]] && item="\"$item\""
		out+="${out:+, }$item"
	done
	printf '%s' "$out"
}

firewall_rules() {
	local trusted=(lo) private=() v4=() v6=() cidr
	[[ -n $WG_ADDRESS ]] && private+=(wg0)
	private+=("${PRIVATE_IFS[@]}")
	trusted+=("${private[@]}")
	local ports="$GAME_LO-$GAME_HI"
	cat <<EOF
# Hallow node firewall, written by deploy/node/install.sh. Only this table is
# touched: Docker's own tables stay as they are.
table inet hallow {}
delete table inet hallow
table inet hallow {
	chain input {
		type filter hook input priority filter; policy drop;
		ct state invalid drop
		ct state established,related accept
		iifname "lo" accept
		meta l4proto { icmp, ipv6-icmp } accept
		udp dport { 68, 546 } accept
EOF
	[[ -n $WG_ADDRESS ]] && printf '\t\tudp dport %s accept\n' "$WG_PORT"
	if ((${#SSH_ALLOW[@]})); then
		for cidr in "${SSH_ALLOW[@]}"; do
			if [[ $cidr == *:* ]]; then v6+=("$cidr"); else v4+=("$cidr"); fi
		done
		((${#v4[@]})) && printf '\t\tip saddr { %s } tcp dport %s accept\n' "$(nft_list 0 "${v4[@]}")" "$SSH_PORT"
		((${#v6[@]})) && printf '\t\tip6 saddr { %s } tcp dport %s accept\n' "$(nft_list 0 "${v6[@]}")" "$SSH_PORT"
	else
		printf '\t\ttcp dport %s accept\n' "$SSH_PORT"
	fi
	if ((${#private[@]})); then
		printf '\t\tiifname { %s } tcp dport { 8080, 2022 } accept\n' "$(nft_list 1 "${private[@]}")"
		printf '\t\tiifname { %s } meta l4proto { tcp, udp } th dport %s accept\n' "$(nft_list 1 "${private[@]}")" "$ports"
	fi
	v4=() v6=()
	for cidr in "${WINGS_ALLOW[@]}"; do
		if [[ $cidr == *:* ]]; then v6+=("$cidr"); else v4+=("$cidr"); fi
	done
	((${#v4[@]})) && printf '\t\tip saddr { %s } tcp dport { 8080, 2022 } accept\n' "$(nft_list 0 "${v4[@]}")"
	((${#v6[@]})) && printf '\t\tip6 saddr { %s } tcp dport { 8080, 2022 } accept\n' "$(nft_list 0 "${v6[@]}")"
	cat <<EOF
	}

	# Game ports are published by Docker, which forwards before input: drop
	# them here, ahead of Docker's DNAT, unless they came in privately or
	# from a container on this node.
	chain game_ports {
		type filter hook prerouting priority mangle; policy accept;
		ct state established,related accept
		iifname { $(nft_list 1 "${trusted[@]}") } accept
		iifname { "docker0", "pterodactyl0" } accept
		iifname "br-*" accept
		meta l4proto { tcp, udp } th dport $ports drop
	}
}
EOF
}

step_firewall() {
	if [[ $NO_FIREWALL == 1 ]]; then
		log "firewall: --no-firewall, skipped"
		return 0
	fi
	log "firewall (nftables table inet hallow)"
	if command -v ufw >/dev/null 2>&1 && ufw status 2>/dev/null | grep -q 'Status: active'; then
		log "ufw is active and would fight this firewall: disabling it"
		run ufw --force disable
	fi
	if [[ -n ${SSH_CLIENT:-} && ${#SSH_ALLOW[@]} -gt 0 ]]; then
		log "SSH is limited to ${SSH_ALLOW[*]}; you are connected from ${SSH_CLIENT%% *}"
	fi
	local rules=/etc/hallow/firewall.nft changed=0
	firewall_rules | write_file "$rules" 0644 && changed=1
	write_file /etc/systemd/system/hallow-firewall.service 0644 <<EOF && changed=1
[Unit]
Description=Hallow node firewall (nftables table inet hallow)
Wants=network-pre.target
Before=network-pre.target docker.service wg-quick@wg0.service
DefaultDependencies=no

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/usr/sbin/nft -f $rules
ExecReload=/usr/sbin/nft -f $rules
ExecStop=/usr/sbin/nft delete table inet hallow

[Install]
WantedBy=multi-user.target
EOF
	if [[ $DRY_RUN != 1 ]]; then
		nft -c -f "$rules" || die "nft rejected $rules"
	fi
	run systemctl daemon-reload
	run systemctl enable hallow-firewall.service
	if ((changed)); then
		run systemctl restart hallow-firewall.service
	else
		run systemctl start hallow-firewall.service
	fi
}

step_wings() {
	log "Calagopus Wings $WINGS_VERSION ($ARCH)"
	local bin=/usr/local/bin/wings
	if [[ -x $bin ]] && sha256sum "$bin" 2>/dev/null | grep -q "^$WINGS_SHA256 "; then
		log "Wings is already this build"
	elif [[ $DRY_RUN == 1 ]]; then
		printf '+ curl -fsSL %s -o <tmp>\n' "$WINGS_URL"
		printf '+ check sha256 %s\n' "$WINGS_SHA256"
		printf '+ install -m 0755 <tmp> %s\n' "$bin"
	else
		local tmp
		tmp=$(mktemp)
		curl -fsSL --retry 5 --retry-connrefused "$WINGS_URL" -o "$tmp"
		if ! printf '%s  %s\n' "$WINGS_SHA256" "$tmp" | sha256sum -c --quiet -; then
			rm -f "$tmp"
			die "checksum mismatch for $WINGS_URL"
		fi
		install -m 0755 "$tmp" "$bin"
		rm -f "$tmp"
	fi
	# The unit `wings service-install` writes, plus ordering after the private
	# network and a higher file limit. Written here because service-install
	# also starts Wings, which without a config means pairing mode.
	local after="docker.service hallow-firewall.service" wants=""
	if [[ -n $WG_ADDRESS ]]; then
		after+=" wg-quick@wg0.service"
		wants=" wg-quick@wg0.service"
	fi
	write_file /etc/systemd/system/wings.service 0644 <<EOF || true
[Unit]
Description=Calagopus Wings Daemon
After=$after
Requires=docker.service
Wants=hallow-firewall.service$wants
PartOf=docker.service

[Service]
User=root
KillMode=process
LimitNOFILE=1048576
PIDFile=/run/calagopus-wings/daemon.pid
ExecStart=$bin
Restart=on-failure
StartLimitInterval=180
StartLimitBurst=30
RestartSec=5s

[Install]
WantedBy=multi-user.target
EOF
	run systemctl daemon-reload
}

json_field() { jq -er --arg f "$1" '.[$f]' <<<"$2"; }

step_register() {
	local config=/etc/calagopus-wings/config.yml
	case $REGISTER in
	skip) log "registration: skipped"; return 0 ;;
	esac
	if [[ -s $config && $REJOIN != 1 ]]; then
		log "Wings is already registered ($config); --rejoin to do it again"
		return 0
	fi
	local panel=$PANEL_URL code=$ENROLL_CODE
	if [[ $REGISTER == join ]]; then
		log "joining through $CONTROL_PLANE_URL"
		local wg_pub="" public_ip memory_mb disk_mb body reply
		[[ -s /etc/wireguard/wg0.key ]] && wg_pub=$(wg pubkey </etc/wireguard/wg0.key)
		public_ip=$(ip -4 route get 1.1.1.1 2>/dev/null | sed -n 's/.* src \([0-9.]*\).*/\1/p' || true)
		memory_mb=$(awk '/^MemTotal:/ { print int($2 / 1024) }' /proc/meminfo 2>/dev/null || echo 0)
		disk_mb=$(df -Pm /var/lib 2>/dev/null | awk 'NR == 2 { print $2 }' || echo 0)
		body=$(jq -n --arg hostname "$(hostname)" --arg private_ip "$PRIVATE_IP" \
			--arg public_ip "$public_ip" --arg wg "$wg_pub" --arg arch "$ARCH" \
			--argjson memory_mb "${memory_mb:-0}" --argjson disk_mb "${disk_mb:-0}" \
			'{hostname: $hostname, private_ip: $private_ip, arch: $arch,
			  memory_mb: $memory_mb, disk_mb: $disk_mb,
			  public_ip: (if $public_ip == "" then null else $public_ip end),
			  wireguard_public_key: (if $wg == "" then null else $wg end)}')
		if [[ $DRY_RUN == 1 ]]; then
			printf '+ POST %s/v1/nodes/join (Authorization: Bearer <join token>)\n' "${CONTROL_PLANE_URL%/}"
			show <<<"$body"
			panel="<panel url from the reply>" code="<enrollment code from the reply>"
		else
			# Retries only cover not reaching the control plane, or a 5xx/429:
			# a failed join gives the token back, so trying again is safe.
			reply=$(curl -fsS --retry 10 --retry-connrefused --retry-delay 3 \
				-H "Authorization: Bearer $JOIN_TOKEN" -H 'content-type: application/json' \
				--data-binary "$body" "${CONTROL_PLANE_URL%/}/v1/nodes/join") ||
				die "the control plane refused the join (token used, expired or wrong?)"
			panel=$(json_field panel_url "$reply") || die "no panel_url in the join reply"
			code=$(json_field enroll_code "$reply") || die "no enroll_code in the join reply"
			log "registered as $(json_field name "$reply" || echo '?') in the panel"
		fi
	fi
	log "enrolling Wings with $panel"
	if [[ $DRY_RUN == 1 ]]; then
		printf '+ wings configure --override --panel-url %s --enroll <code>\n' "$panel"
	else
		wings configure --override --panel-url "$panel" --enroll "$code" ||
			die "Wings couldn't enroll with the panel"
	fi
	local labels_json
	labels_json=$(printf '%s\n' "${LABELS[@]}" | jq -R 'select(length > 0) | split("=") | {(.[0]): (.[1:] | join("="))}' | jq -s 'add // {}')
	jq -n --arg role "$ROLE" --arg region "$REGION" --arg private_ip "$PRIVATE_IP" \
		--argjson labels "$labels_json" --arg wings "$WINGS_VERSION" \
		'{role: $role, region: $region, private_ip: $private_ip, labels: $labels, wings_version: $wings}' |
		write_file /etc/hallow/node.json 0644 || true
	# The token is spent; don't leave it lying around.
	if [[ -n $ENV_FILE && -f $ENV_FILE ]] && grep -q '^HALLOW_\(JOIN_TOKEN\|ENROLL_CODE\)=' "$ENV_FILE"; then
		run sed -i -e '/^HALLOW_JOIN_TOKEN=/d' -e '/^HALLOW_ENROLL_CODE=/d' "$ENV_FILE"
	fi
}

step_start() {
	if [[ $REGISTER == skip && ! -s /etc/calagopus-wings/config.yml ]]; then
		log "Wings is installed but not registered, so not started"
		return 0
	fi
	log "starting Wings"
	run systemctl enable wings
	run systemctl restart wings
}

[[ $DRY_RUN == 1 ]] && log "dry run: nothing is changed"
log "Hallow node: $OS_ID $OS_VERSION, $ARCH, role $ROLE, region ${REGION:-unset}, register by $REGISTER"
step_packages
step_docker
step_sysctl
step_wireguard
step_firewall
step_wings
step_register
step_start
log "done"

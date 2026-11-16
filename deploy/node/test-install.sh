#!/usr/bin/env bash
# Dry-run tests for install.sh: runs it against fake /etc/os-release files and
# checks what it would do. Changes nothing on this machine; needs bash 4+ and
# jq. Run from anywhere: deploy/node/test-install.sh
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
script=$here/install.sh
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

os() { printf 'ID=%s\nVERSION_ID="%s"\nVERSION_CODENAME=%s\n' "$1" "$2" "$3" >"$work/os-$1-$2"; }
os debian 12 bookworm
os debian 13 trixie
os ubuntu 24.04 noble
os ubuntu 22.04 jammy
os centos 9 ""

PEER='xTIBA5rboUvnH4htodjb6e697QjLERt1NAB4mZqp8Dg=|hub.example.net:51820|10.42.0.0/16|25'
JOIN=(--control-plane https://cp.example.net --join-token hjt_test --region fsn1
	--wg-address 10.42.1.7/16 --wg-peer "$PEER")

failures=0 out=""
pass() { printf 'ok   %s\n' "$1"; }
fail() { printf 'FAIL %s\n' "$1"; printf '%s\n' "$out" | sed 's/^/     /' | tail -n 20; failures=$((failures + 1)); }

# expect NAME OS_FILE EXIT_CODE PATTERN... -- ARGS...
expect() {
	local name=$1 os_file=$2 want=$3 patterns=() code=0
	shift 3
	while [[ $1 != -- ]]; do patterns+=("$1"); shift; done
	shift
	out=$(env -i PATH="$PATH" HOME="$work" HALLOW_OS_RELEASE="$work/$os_file" \
		bash "$script" --dry-run "$@" 2>&1) || code=$?
	if [[ $code != "$want" ]]; then
		fail "$name: exit $code, wanted $want"
		return
	fi
	local p
	for p in "${patterns[@]}"; do
		if ! grep -qF -- "$p" <<<"$out"; then
			fail "$name: no '$p' in the output"
			return
		fi
	done
	pass "$name"
}

expect "debian 12 joins" os-debian-12 0 \
	"download.docker.com/linux/debian bookworm stable" \
	"wings-rs-" "check sha256" \
	"Address = 10.42.1.7/16" "PrivateKey = <redacted>" "Endpoint = hub.example.net:51820" \
	"policy drop" "th dport 30000-30999 drop" 'iifname { "wg0" } tcp dport { 8080, 2022 } accept' \
	"tcp dport 22 accept" \
	"POST https://cp.example.net/v1/nodes/join" '"private_ip": "10.42.1.7"' \
	"wings configure --override" "systemctl restart wings" "==> done" \
	-- "${JOIN[@]}"
expect "debian 13" os-debian-13 0 "linux/debian trixie stable" -- "${JOIN[@]}"
expect "ubuntu 24.04" os-ubuntu-24.04 0 "linux/ubuntu noble stable" -- "${JOIN[@]}"
expect "ubuntu 22.04 refused" os-ubuntu-22.04 1 "unsupported OS ubuntu 22.04" -- "${JOIN[@]}"
expect "centos refused" os-centos-9 1 "unsupported OS centos 9" -- "${JOIN[@]}"
expect "no os-release refused" os-missing 1 "only Debian 12/13 and Ubuntu 24.04" -- "${JOIN[@]}"

expect "join token never printed" os-debian-12 0 "Bearer <join token>" -- "${JOIN[@]}"
if grep -q hjt_test <<<"$out"; then fail "the join token leaked into the output"; else pass "token redacted"; fi

expect "panel enrollment code" os-debian-12 0 \
	"wings configure --override --panel-url https://panel.example.net --enroll <code>" \
	"enrolling Wings with https://panel.example.net" \
	-- --panel-url https://panel.example.net --enroll-code abc123
expect "no firewall" os-debian-12 0 "firewall: --no-firewall, skipped" \
	-- --panel-url https://panel.example.net --enroll-code abc123 --no-firewall
expect "skip register leaves wings stopped" os-debian-12 0 "not registered, so not started" \
	-- --skip-register
expect "ssh restricted" os-debian-12 0 \
	"ip saddr { 203.0.113.0/24, 198.51.100.7 } tcp dport 2222 accept" \
	"ip6 saddr { 2001:db8::/32 } tcp dport 2222 accept" \
	-- "${JOIN[@]}" --ssh-allow 203.0.113.0/24 --ssh-allow 198.51.100.7 --ssh-allow 2001:db8::/32 --ssh-port 2222
expect "private interfaces and wings-allow" os-debian-12 0 \
	'iifname { "lo", "wg0", "ens10" } accept' \
	"ip saddr { 10.0.0.2/32 } tcp dport { 8080, 2022 } accept" \
	-- "${JOIN[@]}" --private-if ens10 --wings-allow 10.0.0.2/32
expect "game ports" os-debian-12 0 "th dport 25600-25700 drop" -- "${JOIN[@]}" --game-ports 25600-25700
expect "labels" os-debian-12 0 '"pool": "games"' '"tier": "a"' \
	-- "${JOIN[@]}" --label pool=games --label tier=a --role base

expect "bad role" os-debian-12 1 "--role must be base or burst" -- "${JOIN[@]}" --role huge
expect "token needs control plane" os-debian-12 1 "--join-token needs --control-plane" -- --join-token x --wg-address 10.42.1.7/16 --wg-peer "$PEER"
expect "nothing to register with" os-debian-12 1 "give --join-token" -- --region fsn1
expect "wg address needs a peer" os-debian-12 1 "needs at least one --wg-peer" -- --skip-register --wg-address 10.42.1.7/16
expect "bad peer" os-debian-12 1 "bad --wg-peer" -- --skip-register --wg-address 10.42.1.7/16 --wg-peer 'nope|x|y'
expect "game ports overlap ephemeral" os-debian-12 1 "--game-ports must be within" -- "${JOIN[@]}" --game-ports 39000-41000
expect "custom wings needs a checksum" os-debian-12 1 "needs --wings-sha256" -- "${JOIN[@]}" --wings-version 1.3.0
expect "custom wings url" os-debian-12 0 "https://example.net/wings/1.3.0/wings-" \
	-- "${JOIN[@]}" --wings-version 1.3.0 --wings-url 'https://example.net/wings/{version}/wings-{arch}' \
	--wings-sha256 0000000000000000000000000000000000000000000000000000000000000000
expect "unknown flag" os-debian-12 1 "unknown option --frobnicate" -- --frobnicate

# The env file cloud-init writes: literal values, flags win over it.
cat >"$work/node.env" <<EOF
# written by cloud-init
HALLOW_CONTROL_PLANE_URL=https://cp.example.net
HALLOW_JOIN_TOKEN=hjt_from_file
HALLOW_ROLE=base
HALLOW_REGION=nbg1
HALLOW_LABELS=pool=games tier=a
HALLOW_WG_ADDRESS=10.42.2.9/16
HALLOW_WG_PEERS=$PEER
HALLOW_SSH_ALLOW=203.0.113.0/24
EOF
expect "env file" os-debian-12 0 "role base, region nbg1" '"private_ip": "10.42.2.9"' '"tier": "a"' \
	"ip saddr { 203.0.113.0/24 } tcp dport 22 accept" \
	-- --env-file "$work/node.env"
expect "flags override the env file" os-debian-12 0 "region hel1" -- --env-file "$work/node.env" --region hel1
printf 'rm -rf /\n' >"$work/evil.env"
expect "env file is not evaluated" os-debian-12 1 "unexpected line" -- --env-file "$work/evil.env"

if ((failures)); then
	printf '%d failed\n' "$failures"
	exit 1
fi
printf 'all passed\n'

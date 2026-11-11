#!/bin/bash
# vm-boot-capture.sh - record what metalog did during a boot, on a TEST VM
#
# 20261111 - Created by Claude (claude.ai, Anthropic) - scottfk
# 20261111 - Fix set -e exit after the log copy loop - Claude - scottfk
#
# Usage (as root, in the test VM, after a boot with metalog in a runlevel):
#   tools/vm-boot-capture.sh LABEL     capture into /root/boots/LABEL
#   tools/vm-boot-capture.sh --clean   remove metalog's log dirs before a reboot
#   tools/vm-boot-capture.sh --compare A B   diff two captures, normalized
#
# A capture holds:
# - the log directories from /etc/metalog.conf;
# - the process titles and the console log level while metalog runs;
# - a few test messages, including `rc-service metalog buffer/unbuffer`;
# - the console level after `rc-service metalog stop`.
# It stops metalog at the end, so everything is flushed.
#
# --compare drops what legitimately differs between boots: timestamps,
# kernel [uptime] stamps, pids and the .timestamp files.  It prints the
# lines unique to each side.

set -euo pipefail

logdirs() {
	sed -nE 's/^[[:space:]]*logdir[[:space:]]*=[[:space:]]*"?([^"]*)"?[[:space:]]*$/\1/p' /etc/metalog.conf | sort -u
}

normalize() {
	sed -E \
		-e 's/^[A-Z][a-z]{2} [ 0-9][0-9] [0-9]{2}:[0-9]{2}:[0-9]{2} //' \
		-e 's/\[ *[0-9]+\.[0-9]+\]//g' \
		-e 's/\[[0-9]+\]/[PID]/g' \
		-e 's/(Child|child) \[[0-9]+\]/\1 [PID]/g' \
		-e 's/pid [0-9]+/pid PID/g'
}

case ${1:-} in
--clean)
	logdirs | while read -r d; do rm -rf -- "$d"; done
	exit 0
	;;
--compare)
	a=/root/boots/$2 b=/root/boots/$3
	for f in $( (cd "$a" && find . -type f; cd "$b" && find . -type f) | grep -v '/\.timestamp$' | sort -u); do
		if [[ ! -f $a/$f || ! -f $b/$f ]]; then
			echo "== $f: only in $([[ -f $a/$f ]] && echo "$2" || echo "$3")"
			continue
		fi
		d=$(diff <(normalize < "$a/$f" | sort) <(normalize < "$b/$f" | sort) || true)
		[[ -n $d ]] && printf '== %s\n%s\n' "$f" "$d"
	done
	exit 0
	;;
'' | -*)
	echo "usage: $0 LABEL | --clean | --compare A B" >&2
	exit 2
	;;
esac

out=/root/boots/$1
rm -rf "$out"
mkdir -p "$out/logs"
for i in $(seq 30); do
	rc-service metalog status >/dev/null 2>&1 && break
	sleep 1
done
sleep 3
{
	echo "status: $(rc-service metalog status 2>&1 | tail -1)"
	echo "titles: $(pgrep -a metalog | sed 's/^[0-9]* //' | sort | tr '\n' '|')"
	echo "printk while running: $(cat /proc/sys/kernel/printk)"
} > "$out/info"

logger -t boottest "plain message"
logger -t boottest -p auth.err "authentication failure for testuser"
rc-service metalog buffer
logger -t boottest "while buffered"
rc-service metalog unbuffer
logger -t boottest "after unbuffer"
echo "<6>boottest kernel line" > /dev/kmsg
sleep 1

rc-service metalog stop
echo "printk after stop: $(cat /proc/sys/kernel/printk)" >> "$out/info"
logdirs | while read -r d; do
	if [[ -d $d ]]; then cp -a -- "$d" "$out/logs/"; fi
done
rc-service metalog start >/dev/null 2>&1 || true
cat "$out/info"

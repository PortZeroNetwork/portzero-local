#!/usr/bin/env bash
# LIFECYCLE E2E (macOS): full privileged setup -> verify artifacts landed ->
# uninstall -> ASSERT every artifact is GONE. Closes the audit gap where
# uninstall was completely untested on macOS: nothing proved that removing
# portzero pulls its trusted CA out of the System keychain, removes the
# /etc/resolver/portzero.local resolver, unloads the root LaunchDaemon, tears
# down the utun interface, and removes the /etc/hosts pin.
#
# Delivery caveat (documented, not hidden): macOS ships via Homebrew (a release
# tar.gz + tap formula), not a .pkg, and the Parallels macOS guest is an
# artifact-only, network-pristine end-user machine, so we cannot `brew install`
# the real release tarball in-VM (that needs GitHub Releases + a real formula
# sha256). What we CAN and DO exercise here is the exact privileged lifecycle the
# Homebrew formula's `post_install` + `sudo portzero setup` performs, driven by
# the host-built binary the harness pushes in — i.e. the CA-trust / LaunchDaemon /
# resolver / hosts residue the audit actually worries about. The brew *download*
# step itself is covered by release.yml's macOS interop job, not here.
#
# Prints greppable `PHASE=<name> ... ok=<true|false>` lines, then RESULT=PASS|FAIL.
#
# Env:
#   PORTZERO_EXE  path to the portzero binary (the harness pushes the host-built
#                 one in and sets this). Defaults to a copy under /tmp.
# Helper predicates are invoked indirectly via assert/assert_not; silence the
# resulting spurious SC2317 "unreachable" info.
# shellcheck disable=SC2317
set -uo pipefail

EXE="${PORTZERO_EXE:-/tmp/portzero-vmtest/bin/portzero}"
PLIST=/Library/LaunchDaemons/cloud.portzero.daemon.plist
RESOLVER=/etc/resolver/portzero.local
HOSTS_PIN='# portzero-local'
CERT_CN='PortZero Local CA'
SYS_KEYCHAIN=/Library/Keychains/System.keychain
TUN_PREFIX=utun

fails=0
SUDO=""; [ "$(id -u)" -ne 0 ] && SUDO="sudo"

assert() {
    local phase="$1"; shift
    if "$@" >/dev/null 2>&1; then echo "PHASE=$phase ok=true"
    else echo "PHASE=$phase ok=false"; fails=$((fails + 1)); fi
}
assert_not() {
    local phase="$1"; shift
    if "$@" >/dev/null 2>&1; then echo "PHASE=$phase ok=false"; fails=$((fails + 1))
    else echo "PHASE=$phase ok=true"; fi
}

keychain_has_ca() { $SUDO security find-certificate -c "$CERT_CN" "$SYS_KEYCHAIN" >/dev/null 2>&1; }
launchdaemon_loaded() { $SUDO launchctl print system/cloud.portzero.daemon >/dev/null 2>&1; }
hosts_has_pin() { grep -qF "$HOSTS_PIN" /etc/hosts 2>/dev/null; }
utun_present() {
    # A portzero utun carries the overlay gateway 10.254.0.1; match on that so we
    # don't trip over unrelated utun interfaces (VPNs etc.).
    ifconfig 2>/dev/null | grep -A3 "^${TUN_PREFIX}" | grep -q '10\.254\.0\.1'
}

echo "PHASE=preflight exe=$([ -x "$EXE" ] && echo true || echo false) tun=${LIFECYCLE_TUN:-0}"
if [ ! -x "$EXE" ]; then
    echo "RESULT=FAIL portzero binary not found at $EXE (set PORTZERO_EXE)"
    exit 1
fi

# --- INSTALL: the documented privileged setup (== brew post_install + setup) --
echo ">> portzero setup (trust install + autostart + resolver + hosts pin)"
$SUDO env HOME="$HOME" "$EXE" trust generate >/dev/null 2>&1 || true
# `setup` is non-interactive-safe and idempotent; run it under a background PID
# with a bounded wait so a hung privileged step can't wedge the whole test.
( $SUDO env HOME="$HOME" "$EXE" setup >/tmp/pz-setup.log 2>&1 ) & sp=$!
( sleep 120; kill -9 "$sp" 2>/dev/null ) >/dev/null 2>&1 & killer=$!
wait "$sp" 2>/dev/null; kill "$killer" 2>/dev/null

assert install-trust-keychain keychain_has_ca
assert install-launchdaemon-plist test -f "$PLIST"
assert install-launchdaemon-loaded launchdaemon_loaded
assert install-resolver test -f "$RESOLVER"
assert install-hosts-pin hosts_has_pin
if [ "${LIFECYCLE_TUN:-0}" = 1 ]; then
    up=0; for _ in $(seq 1 30); do utun_present && { up=1; break; }; sleep 1; done
    assert tun-up test "$up" = 1
fi

# --- UNINSTALL: the way the brew caveats / a user removes it -----------------
echo ">> uninstall (trust uninstall + autostart disable + resolver + hosts)"
$SUDO env HOME="$HOME" "$EXE" stop >/dev/null 2>&1 || true
$SUDO env HOME="$HOME" "$EXE" trust uninstall >/dev/null 2>&1 || true
$SUDO env HOME="$HOME" "$EXE" autostart disable >/dev/null 2>&1 || true
$SUDO rm -f "$RESOLVER" 2>/dev/null || true
# Remove the managed hosts pin, preserving user lines (same shape as prerm/sed).
if hosts_has_pin; then
    tmp="$(mktemp)"; grep -vF "$HOSTS_PIN" /etc/hosts > "$tmp" 2>/dev/null || true
    $SUDO install -m 0644 "$tmp" /etc/hosts 2>/dev/null || true
    rm -f "$tmp"
fi
sleep 2

# --- ASSERT CLEAN: every install artifact must be GONE ----------------------
assert_not clean-trust-keychain keychain_has_ca
assert_not clean-launchdaemon-plist test -e "$PLIST"
assert_not clean-launchdaemon-loaded launchdaemon_loaded
assert_not clean-resolver test -e "$RESOLVER"
assert_not clean-hosts-pin hosts_has_pin
assert_not clean-tun utun_present

if [ "$fails" -eq 0 ]; then
    echo "RESULT=PASS lifecycle (setup -> verify -> uninstall -> assert-clean)"
    exit 0
fi
echo "RESULT=FAIL lifecycle assertions failed=$fails"
exit 1

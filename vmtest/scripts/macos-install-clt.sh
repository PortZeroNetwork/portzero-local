#!/usr/bin/env bash
# Install Xcode Command Line Tools headlessly (no GUI, no Apple ID) via
# softwareupdate. Unlike Windows/Linux toolchains, CLT cannot be staged as a
# portable file in the shared cache — Apple only vends it through
# softwareupdate — so the "cache" for macOS is the VM snapshot taken AFTER this
# runs. Idempotent: exits early if CLT already compiles.
set -euo pipefail

if clang -x c -o /tmp/_cltcheck - <<<'int main(){return 0;}' 2>/dev/null; then
    echo "clt_already=yes"; exit 0
fi

# This sentinel makes softwareupdate list the on-demand CLT package.
trigger=/tmp/.com.apple.dt.CommandLineTools.installondemand.in-progress
sudo touch "$trigger"
label=$(softwareupdate --list 2>/dev/null \
    | grep -E 'Label: Command Line Tools' \
    | sed -E 's/.*Label: //' \
    | sort -V | tail -1)
echo "clt_label=${label:-NONE}"
if [ -z "$label" ]; then sudo rm -f "$trigger"; echo "clt_install=NO_PACKAGE"; exit 1; fi

sudo softwareupdate --install "$label" --verbose
sudo rm -f "$trigger"

if clang -x c -o /tmp/_cltcheck - <<<'int main(){return 0;}' 2>/dev/null; then
    echo "clt_install=OK"
else
    echo "clt_install=FAILED_VERIFY"; exit 1
fi

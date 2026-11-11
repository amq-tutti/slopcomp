#!/usr/bin/env bash
# Builds and zips the Windows release (run in MSYS2 UCRT64 after deps/build.sh).
# Fails if either .exe imports a DLL that doesn't ship with Windows.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
cargo build --release -p comp-gui -p comp-cli
REL=target/release
# DLLs every Windows 10+ install has (lower-case).
ALLOWED='^(kernel32|user32|gdi32|advapi32|shell32|ole32|oleaut32|comdlg32|comctl32|ws2_32|bcrypt|bcryptprimitives|crypt32|secur32|ntdll|opengl32|imm32|winmm|dwmapi|uxtheme|shlwapi|version|setupapi|propsys|userenv|dbghelp|ucrtbase|msvcrt|api-ms-win-[a-z0-9-]+|kernelbase|powrprof|hid|cfgmgr32|dxgi|d3d11|d3d12|dcomp|windowscodecs|ntoskrnl|iphlpapi|netapi32|psapi|rpcrt4|synchronization)\.dll$'
bad=0
for exe in slopcomp.exe comp.exe; do
  echo "== $exe imports:"
  for dll in $(objdump -p "$REL/$exe" | sed -n 's/^\s*DLL Name: //p' | tr 'A-Z' 'a-z' | sort -u); do
    if [[ "$dll" =~ $ALLOWED ]]; then echo "   $dll"; else echo "   $dll  <-- NOT a Windows system DLL"; bad=1; fi
  done
done
[[ $bad == 0 ]] || { echo "error: non-system DLL dependencies (static linking incomplete)"; exit 1; }
STAGE="$(mktemp -d)/SlopComp $VERSION"
mkdir -p "$STAGE"
cp "$REL/slopcomp.exe" "$STAGE/SlopComp.exe"
cp "$REL/comp.exe" LICENSE THIRD_PARTY_NOTICES.md "$STAGE/"
ZIP="$ROOT/$REL/SlopComp-$VERSION-windows-x64.zip"
rm -f "$ZIP"
(cd "$(dirname "$STAGE")" && zip -qr "$ZIP" "$(basename "$STAGE")")
echo "built $ZIP ($(du -h "$ZIP" | cut -f1))"

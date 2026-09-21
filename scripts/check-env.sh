#!/usr/bin/env bash
# Report whether this machine can build and run Zoog.
# Exit 0 if all required checks pass; 1 otherwise. Optional checks never fail
# the script.
set -u

pass=0
fail=0

row() { # status name detail
    printf '%-6s %-28s %s\n' "$1" "$2" "$3"
}

ok()   { row "PASS" "$1" "$2"; pass=$((pass + 1)); }
bad()  { row "FAIL" "$1" "$2"; fail=$((fail + 1)); }
warn() { row "WARN" "$1" "$2"; }

printf '%-6s %-28s %s\n' "STATUS" "CHECK" "DETAIL"
printf '%-6s %-28s %s\n' "------" "-----" "------"

# Rust toolchain (required)
if command -v cargo >/dev/null 2>&1; then
    ok "rust-toolchain" "$(rustc --version 2>/dev/null || echo 'rustc missing')"
else
    bad "rust-toolchain" "cargo not found; install via rustup"
fi

# pkg-config (required to find ALSA)
if command -v pkg-config >/dev/null 2>&1; then
    ok "pkg-config" "$(pkg-config --version)"
else
    bad "pkg-config" "not found; install pkg-config"
fi

# ALSA dev headers (required for midir and cpal)
if pkg-config --exists alsa 2>/dev/null; then
    ok "alsa (libasound2-dev)" "alsa $(pkg-config --modversion alsa)"
else
    bad "alsa (libasound2-dev)" "pkg-config cannot find alsa; install libasound2-dev"
fi

# JACK dev headers (optional backend)
if pkg-config --exists jack 2>/dev/null; then
    ok "jack (optional)" "jack $(pkg-config --modversion jack)"
else
    warn "jack (optional)" "not found; only needed for the JACK backend (libjack-jackd2-dev)"
fi

# Vulkan runtime for iced/wgpu (required for the GUI phase)
if command -v vulkaninfo >/dev/null 2>&1; then
    ok "vulkan" "vulkaninfo present"
elif ldconfig -p 2>/dev/null | grep -q libvulkan.so.1; then
    ok "vulkan" "libvulkan.so.1 present (vulkaninfo not installed)"
else
    bad "vulkan" "libvulkan not found; install vulkan drivers/loader for iced"
fi

# Surge XT CLAP plugin (required from Phase 3 on)
surge=""
IFS=':' read -r -a extra_paths <<< "${CLAP_PATH:-}"
for dir in "$HOME/.clap" /usr/lib/clap "${extra_paths[@]}"; do
    [ -n "$dir" ] && [ -d "$dir" ] || continue
    found=$(find "$dir" -iname 'Surge*XT*.clap' -print -quit 2>/dev/null)
    if [ -n "$found" ]; then
        surge="$found"
        break
    fi
done
if [ -n "$surge" ]; then
    ok "surge-xt-clap" "$surge"
else
    bad "surge-xt-clap" "no Surge*XT*.clap in ~/.clap, /usr/lib/clap, or \$CLAP_PATH"
fi

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]

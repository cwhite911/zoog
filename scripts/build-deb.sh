#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# SPDX-FileCopyrightText: 2026 Corey T. White
# Builds the Zoog .deb. Requires cargo-deb.
set -eu
cd "$(dirname "$0")/.."
cargo build --release -p lab-gui -p labctl
cargo deb -p lab-gui --no-build
ls -lh target/debian/*.deb

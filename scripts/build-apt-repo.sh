#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# SPDX-FileCopyrightText: 2026 Corey T. White
#
# Builds a signed apt repository under docs/apt/, which GitHub Pages serves
# alongside the manual. Run build-deb.sh first, or pass --build.
#
# Requires: dpkg-dev (dpkg-scanpackages), apt-utils (apt-ftparchive), gpg,
# and a secret key whose user ID matches $ZOOG_SIGNING_KEY.
set -euo pipefail
cd "$(dirname "$0")/.."

SUITE="${ZOOG_APT_SUITE:-stable}"
COMPONENT=main
ARCH=amd64
SIGNING_KEY="${ZOOG_SIGNING_KEY:-Zoog Archive Signing Key}"
REPO=docs/apt

[ "${1:-}" = "--build" ] && ./scripts/build-deb.sh

if ! compgen -G "target/debian/zoog_*.deb" > /dev/null; then
    echo "no .deb in target/debian; run scripts/build-deb.sh first" >&2
    exit 1
fi
if ! gpg --list-secret-keys "$SIGNING_KEY" > /dev/null 2>&1; then
    echo "no secret key matching '$SIGNING_KEY'." >&2
    echo "Create one with:" >&2
    echo "  gpg --quick-generate-key \"$SIGNING_KEY <zoog@cwhite911.github.io>\" rsa4096 sign 3y" >&2
    exit 1
fi

mkdir -p "$REPO/pool/$COMPONENT/z/zoog" "$REPO/dists/$SUITE/$COMPONENT/binary-$ARCH"
cp -f target/debian/zoog_*.deb "$REPO/pool/$COMPONENT/z/zoog/"

# Regenerate indices from scratch so a stale Release is never re-signed or
# checksummed into its own replacement.
rm -f "$REPO/dists/$SUITE/Release" "$REPO/dists/$SUITE/Release.gpg" \
      "$REPO/dists/$SUITE/InRelease"

pushd "$REPO" > /dev/null

# Package index. Paths inside are relative to the repository root, which is
# what the sources.list entry points at.
dpkg-scanpackages --arch "$ARCH" pool > "dists/$SUITE/$COMPONENT/binary-$ARCH/Packages" 2>/dev/null
gzip -9kf "dists/$SUITE/$COMPONENT/binary-$ARCH/Packages"

# Release file with checksums over the indices.
apt-ftparchive \
    -o "APT::FTPArchive::Release::Origin=Zoog" \
    -o "APT::FTPArchive::Release::Label=Zoog" \
    -o "APT::FTPArchive::Release::Suite=$SUITE" \
    -o "APT::FTPArchive::Release::Codename=$SUITE" \
    -o "APT::FTPArchive::Release::Architectures=$ARCH" \
    -o "APT::FTPArchive::Release::Components=$COMPONENT" \
    -o "APT::FTPArchive::Release::Description=Zoog releases for Debian and Ubuntu derivatives" \
    release "dists/$SUITE" > "dists/$SUITE/Release"

# Both signature forms: InRelease (inline) is what modern apt prefers,
# Release.gpg (detached) keeps older clients working.
gpg --default-key "$SIGNING_KEY" --yes --armor --detach-sign \
    -o "dists/$SUITE/Release.gpg" "dists/$SUITE/Release"
gpg --default-key "$SIGNING_KEY" --yes --clearsign \
    -o "dists/$SUITE/InRelease" "dists/$SUITE/Release"

# Public key for users to trust, in both armoured and binary form.
gpg --armor --export "$SIGNING_KEY" > zoog-archive-keyring.asc
gpg --export "$SIGNING_KEY" > zoog-archive-keyring.gpg

popd > /dev/null

echo "repository written to $REPO"
ls -1 "$REPO/pool/$COMPONENT/z/zoog/"
gpg --verify "$REPO/dists/$SUITE/InRelease" 2>&1 | sed -n '1,2p'

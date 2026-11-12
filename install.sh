#!/bin/sh
# Install a prebuilt sooth binary from GitHub Releases.
#
#   curl -fsSL https://raw.githubusercontent.com/Bramvzw/sooth/main/install.sh | sh
#
# Environment:
#   SOOTH_VERSION      a release tag, e.g. v0.2.0 (default: the latest release)
#   SOOTH_INSTALL_DIR  where the binary goes (default: ~/.local/bin)
#
# The archive's SHA-256 is checked against the checksum file published next
# to it before anything is installed. No Rust toolchain needed; for one,
# `cargo install sooth` builds from source instead.
set -eu

repo="Bramvzw/sooth"
install_dir="${SOOTH_INSTALL_DIR:-$HOME/.local/bin}"
# Overridable only so the installer can be tested against a local server.
download_base="${SOOTH_DOWNLOAD_BASE:-https://github.com/$repo/releases}"

fail() {
    printf 'sooth install: %s\n' "$1" >&2
    exit 1
}

need() {
    command -v "$1" >/dev/null 2>&1 || fail "needs \`$1\`, which is not installed"
}

need curl
need tar
need uname

case "$(uname -s)" in
    Linux) os="unknown-linux-musl" ;;
    Darwin) os="apple-darwin" ;;
    *) fail "no prebuilt binary for $(uname -s) — use \`cargo install sooth\`" ;;
esac
case "$(uname -m)" in
    x86_64 | amd64) arch="x86_64" ;;
    arm64 | aarch64) arch="aarch64" ;;
    *) fail "no prebuilt binary for $(uname -m) — use \`cargo install sooth\`" ;;
esac
target="$arch-$os"

if [ -n "${SOOTH_VERSION:-}" ]; then
    tag="$SOOTH_VERSION"
else
    # The redirect target of /releases/latest names the newest tag, without
    # calling the rate-limited API.
    latest="$(curl -fsSLI -o /dev/null -w '%{url_effective}' "$download_base/latest")" ||
        fail "could not find the latest release"
    tag="${latest##*/}"
fi
case "$tag" in
    v*) ;;
    *) fail "unexpected release tag \`$tag\`" ;;
esac

archive="sooth-${tag#v}-$target.tar.gz"
url="$download_base/download/$tag/$archive"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

printf 'Downloading sooth %s for %s\n' "$tag" "$target"
curl -fsSL "$url" -o "$tmp/$archive" || fail "could not download $url"
curl -fsSL "$url.sha256" -o "$tmp/$archive.sha256" || fail "could not download $url.sha256"

expected="$(cut -d ' ' -f 1 <"$tmp/$archive.sha256")"
if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$tmp/$archive" | cut -d ' ' -f 1)"
elif command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "$tmp/$archive" | cut -d ' ' -f 1)"
else
    fail "needs \`sha256sum\` or \`shasum\` to verify the download"
fi
[ -n "$expected" ] && [ "$expected" = "$actual" ] ||
    fail "checksum mismatch for $archive — not installing"

tar -xzf "$tmp/$archive" -C "$tmp"
mkdir -p "$install_dir"
cp "$tmp/sooth-${tag#v}-$target/sooth" "$install_dir/sooth"
chmod 755 "$install_dir/sooth"

printf 'Installed %s\n' "$("$install_dir/sooth" --version)"
case ":$PATH:" in
    *":$install_dir:"*) ;;
    *) printf 'Add %s to your PATH to run `sooth` from anywhere.\n' "$install_dir" ;;
esac

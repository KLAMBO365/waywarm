#!/bin/sh

set -eu

repository="KLAMBO365/waywarm"
install_dir="${HOME:-}/.local/bin"
staged=""

say() {
    printf '%s\n' "$*"
}

die() {
    printf 'waywarm installer: %s\n' "$*" >&2
    exit 1
}

cleanup() {
    if [ -n "${staged:-}" ]; then
        rm -f "$staged"
    fi
    if [ -n "${temporary:-}" ]; then
        rm -rf "$temporary"
    fi
}

trap cleanup 0
trap 'exit 1' 1 2 15

[ -n "${HOME:-}" ] || die 'HOME is not set'

operating_system=$(uname -s 2>/dev/null) || die 'could not detect the operating system'
[ "$operating_system" = "Linux" ] || die "unsupported operating system: $operating_system (Linux is required)"

architecture=$(uname -m 2>/dev/null) || die 'could not detect the CPU architecture'
case "$architecture" in
    x86_64 | amd64)
        target="x86_64-unknown-linux-gnu"
        ;;
    *)
        die "unsupported CPU architecture: $architecture (x86_64 is required)"
        ;;
esac

for command_name in curl tar sha256sum install mktemp mv rm; do
    command -v "$command_name" >/dev/null 2>&1 \
        || die "required command not found: $command_name"
done

latest_url=$(curl --proto '=https' --tlsv1.2 -fsSL \
    -o /dev/null -w '%{url_effective}' \
    "https://github.com/$repository/releases/latest") \
    || die 'could not resolve the latest release'

tag=${latest_url##*/}
case "$tag" in
    v*) version=${tag#v} ;;
    *) die "unexpected latest release tag: $tag" ;;
esac
case "$version" in
    '' | *[!0-9.]*) die "unexpected latest release tag: $tag" ;;
esac

old_ifs=$IFS
IFS=.
set -- $version
IFS=$old_ifs
[ "$#" -eq 3 ] || die "unexpected latest release tag: $tag"
for component in "$@"; do
    case "$component" in
        '' | *[!0-9]*) die "unexpected latest release tag: $tag" ;;
    esac
done

package="waywarm-$tag-$target"
archive="$package.tar.gz"
checksum="$archive.sha256"
download_base="https://github.com/$repository/releases/download/$tag"

temporary=$(mktemp -d "${TMPDIR:-/tmp}/waywarm-install.XXXXXX") \
    || die 'could not create a temporary directory'

say "Downloading waywarm $tag..."
curl --proto '=https' --tlsv1.2 -fsSL \
    -o "$temporary/$archive" "$download_base/$archive" \
    || die "could not download $archive"
curl --proto '=https' --tlsv1.2 -fsSL \
    -o "$temporary/$checksum" "$download_base/$checksum" \
    || die "could not download $checksum"

say 'Verifying checksum...'
if ! (cd "$temporary" && sha256sum -c "$checksum"); then
    die 'checksum verification failed; the existing installation was not changed'
fi

tar -xzf "$temporary/$archive" -C "$temporary" \
    || die 'could not extract the release archive'
source_binary="$temporary/$package/waywarm"
[ -f "$source_binary" ] || die 'the release archive does not contain the expected binary'

install -d "$install_dir" || die "could not create $install_dir"
staged="$install_dir/.waywarm-install-$$"
install -m 0755 "$source_binary" "$staged" \
    || die 'could not stage the waywarm binary'
mv -f "$staged" "$install_dir/waywarm" \
    || die "could not install waywarm to $install_dir"
staged=""

say "Installed waywarm $tag to $install_dir/waywarm"
case ":${PATH:-}:" in
    *:"$install_dir":*) ;;
    *)
        say ""
        say "Add $install_dir to PATH before launching waywarm."
        ;;
esac
say "Run 'waywarm' to open the settings interface."
say "Run 'waywarm daemon' to configure automatic startup."

#!/bin/sh

set -eu

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
test_root=$(mktemp -d "${TMPDIR:-/tmp}/waywarm-installer-test.XXXXXX")
trap 'rm -rf "$test_root"' 0
trap 'exit 1' 1 2 15

fixtures="$test_root/fixtures"
mocks="$test_root/mocks"
package="waywarm-v1.2.3-x86_64-unknown-linux-gnu"
archive="$package.tar.gz"
mkdir -p "$fixtures/$package" "$mocks"
printf '#!/bin/sh\nprintf "fixture waywarm\\n"\n' >"$fixtures/$package/waywarm"
chmod 0755 "$fixtures/$package/waywarm"
tar -C "$fixtures" -czf "$fixtures/$archive" "$package"
(cd "$fixtures" && sha256sum "$archive" >"$archive.sha256")

cat >"$mocks/uname" <<'EOF'
#!/bin/sh
case "$1" in
    -s) printf '%s\n' "${MOCK_UNAME_S:-Linux}" ;;
    -m) printf '%s\n' "${MOCK_UNAME_M:-x86_64}" ;;
    *) exit 1 ;;
esac
EOF

cat >"$mocks/curl" <<'EOF'
#!/bin/sh
output=""
write_effective=false
url=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        -o)
            output=$2
            shift 2
            ;;
        -w)
            write_effective=true
            shift 2
            ;;
        --proto)
            shift 2
            ;;
        --tlsv1.2 | -fsSL)
            shift
            ;;
        *)
            url=$1
            shift
            ;;
    esac
done

if [ "$write_effective" = true ]; then
    printf '%s\n' 'https://github.com/KLAMBO365/waywarm/releases/tag/v1.2.3'
else
    cp "$FIXTURES/${url##*/}" "$output"
fi
EOF
chmod 0755 "$mocks/uname" "$mocks/curl"

fail() {
    printf 'installer test: %s\n' "$*" >&2
    exit 1
}

home="$test_root/home"
mkdir -p "$home"
FIXTURES="$fixtures" HOME="$home" PATH="$mocks:$PATH" \
    sh "$project_dir/install.sh" >"$test_root/install.out" 2>"$test_root/install.err"
cmp "$fixtures/$package/waywarm" "$home/.local/bin/waywarm" \
    || fail 'installed binary does not match the release binary'
[ -x "$home/.local/bin/waywarm" ] || fail 'installed binary is not executable'

printf 'existing binary\n' >"$home/.local/bin/waywarm"
printf '%064d  %s\n' 0 "$archive" >"$fixtures/$archive.sha256"
if FIXTURES="$fixtures" HOME="$home" PATH="$mocks:$PATH" \
    sh "$project_dir/install.sh" >"$test_root/checksum.out" 2>"$test_root/checksum.err"; then
    fail 'installer accepted a corrupt checksum'
fi
[ "$(cat "$home/.local/bin/waywarm")" = 'existing binary' ] \
    || fail 'checksum failure replaced the existing binary'
grep -q 'checksum verification failed' "$test_root/checksum.err" \
    || fail 'checksum failure did not explain the error'

if MOCK_UNAME_S=Darwin FIXTURES="$fixtures" HOME="$home" PATH="$mocks:$PATH" \
    sh "$project_dir/install.sh" >"$test_root/os.out" 2>"$test_root/os.err"; then
    fail 'installer accepted an unsupported operating system'
fi
grep -q 'unsupported operating system' "$test_root/os.err" \
    || fail 'unsupported operating system error was not actionable'

if MOCK_UNAME_M=aarch64 FIXTURES="$fixtures" HOME="$home" PATH="$mocks:$PATH" \
    sh "$project_dir/install.sh" >"$test_root/arch.out" 2>"$test_root/arch.err"; then
    fail 'installer accepted an unsupported architecture'
fi
grep -q 'unsupported CPU architecture' "$test_root/arch.err" \
    || fail 'unsupported architecture error was not actionable'

missing_path="$test_root/missing-command"
mkdir -p "$missing_path"
ln -s "$(command -v uname)" "$missing_path/uname"
if HOME="$home" PATH="$missing_path" /bin/sh "$project_dir/install.sh" \
    >"$test_root/dependency.out" 2>"$test_root/dependency.err"; then
    fail 'installer continued without curl'
fi
grep -q 'required command not found: curl' "$test_root/dependency.err" \
    || fail 'missing dependency error was not actionable'

printf 'installer tests passed\n'

#!/usr/bin/env bash
set -euo pipefail
umask 0022

debian_package_version() {
    local release_version="$1"
    local debian_version="${release_version/-/\~}-1"

    if ! dpkg --validate-version "${debian_version}"; then
        echo "Invalid Debian package version: ${debian_version}" >&2
        return 1
    fi
    printf '%s\n' "${debian_version}"
}

if [[ "${1:-}" == "--print-version" ]]; then
    if [[ $# -ne 2 ]]; then
        echo "Usage: $0 --print-version <release-version>" >&2
        exit 2
    fi
    debian_package_version "$2"
    exit
fi

if [[ $# -ne 4 ]]; then
    echo "Usage: $0 <version> <binary> <third-party-licenses> <output-directory>" >&2
    exit 2
fi

release_version="$1"
binary="$2"
third_party_licenses="$3"
output_dir="$4"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
resources_dir="$(cd "${script_dir}/../.." && pwd)"
repo_root="$(cd "${resources_dir}/.." && pwd)"

if [[ ! -f "${binary}" ]]; then
    echo "Spotlit binary does not exist: ${binary}" >&2
    exit 1
fi
if [[ ! -f "${third_party_licenses}" ]]; then
    echo "Third-party license report does not exist: ${third_party_licenses}" >&2
    exit 1
fi
debian_version="$(debian_package_version "${release_version}")"
if [[ "$(dpkg --print-architecture)" != "amd64" ]]; then
    echo "The current package definition only supports amd64." >&2
    exit 1
fi
if [[ ! "${SOURCE_DATE_EPOCH:-}" =~ ^[0-9]+$ ]]; then
    echo "SOURCE_DATE_EPOCH must be set to a Unix timestamp." >&2
    exit 1
fi
if ! date --utc --date="@${SOURCE_DATE_EPOCH}" >/dev/null 2>&1; then
    echo "SOURCE_DATE_EPOCH is outside the range supported by date." >&2
    exit 1
fi

package_root="$(mktemp -d "${TMPDIR:-/tmp}/spotlit-deb.XXXXXX")"
shlibdeps_root="$(mktemp -d "${TMPDIR:-/tmp}/spotlit-shlibdeps.XXXXXX")"
trap 'rm -rf "${package_root}" "${shlibdeps_root}"' EXIT
chmod 0755 "${package_root}" "${shlibdeps_root}"

install -Dm755 "${binary}" "${package_root}/usr/bin/spotlit"
install -Dm644 \
    "${resources_dir}/packaging/linux/spotlit.desktop" \
    "${package_root}/usr/share/applications/spotlit.desktop"
install -Dm644 \
    "${resources_dir}/packaging/linux/io.github.oksyd.spotlit.metainfo.xml" \
    "${package_root}/usr/share/metainfo/io.github.oksyd.spotlit.metainfo.xml"
install -Dm644 \
    "${resources_dir}/icons/spotlit-logo.svg" \
    "${package_root}/usr/share/icons/hicolor/scalable/apps/spotlit.svg"
install -Dm644 "${repo_root}/README.md" "${package_root}/usr/share/doc/spotlit/README.md"
install -Dm644 \
    "${third_party_licenses}" \
    "${package_root}/usr/share/doc/spotlit/THIRD-PARTY-LICENSES.txt"
gzip -9n "${package_root}/usr/share/doc/spotlit/THIRD-PARTY-LICENSES.txt"
install -Dm644 \
    "${resources_dir}/packaging/debian/copyright" \
    "${package_root}/usr/share/doc/spotlit/copyright"

install -Dm644 \
    "${resources_dir}/packaging/linux/spotlit.1" \
    "${package_root}/usr/share/man/man1/spotlit.1"
gzip -9n "${package_root}/usr/share/man/man1/spotlit.1"

build_date="$(LC_ALL=C date --utc --date="@${SOURCE_DATE_EPOCH}" --rfc-email)"
changelog_path="${package_root}/usr/share/doc/spotlit/changelog.Debian"
{
    printf 'spotlit (%s) unstable; urgency=medium\n\n' "${debian_version}"
    printf '  * Release Spotlit %s.\n\n' "${release_version}"
    printf ' -- oksyd <oksyd@users.noreply.github.com>  %s\n' "${build_date}"
} > "${changelog_path}"
gzip -9n "${changelog_path}"

install -Dm755 "${binary}" "${shlibdeps_root}/debian/spotlit/usr/bin/spotlit"
install -d -m755 "${shlibdeps_root}/debian/spotlit/DEBIAN"
install -d -m755 "${shlibdeps_root}/debian"
cat > "${shlibdeps_root}/debian/control" <<EOF
Source: spotlit
Section: utils
Priority: optional
Maintainer: oksyd <oksyd@users.noreply.github.com>
Standards-Version: 4.7.0

Package: spotlit
Architecture: any
Description: wallpaper companion for Windows Spotlight and Bing
EOF

shlibdeps_output="$(
    cd "${shlibdeps_root}"
    dpkg-shlibdeps -O -edebian/spotlit/usr/bin/spotlit
)"
shlibdeps="${shlibdeps_output#shlibs:Depends=}"
if [[ -z "${shlibdeps}" || "${shlibdeps}" == "${shlibdeps_output}" ]]; then
    echo "dpkg-shlibdeps did not produce a dependency list." >&2
    exit 1
fi

installed_size="$(du -sk "${package_root}/usr" | cut -f1)"
install -d -m755 "${package_root}/DEBIAN"
cat > "${package_root}/DEBIAN/control" <<EOF
Package: spotlit
Version: ${debian_version}
Section: utils
Priority: optional
Architecture: amd64
Installed-Size: ${installed_size}
Maintainer: oksyd <oksyd@users.noreply.github.com>
Depends: ${shlibdeps}, libglib2.0-bin, libx11-6, libxkbcommon0, libwayland-client0, xdg-utils, zenity
Recommends: gnome-shell
Homepage: https://github.com/oksyd/spotlit
Description: wallpaper companion for Windows Spotlight and Bing
 Spotlit maintains a local wallpaper library and can apply wallpapers
 manually or on a configurable schedule on supported desktops.
EOF
chmod 0644 "${package_root}/DEBIAN/control"

find "${package_root}" -exec \
    touch --no-dereference --date="@${SOURCE_DATE_EPOCH}" {} +

mkdir -p "${output_dir}"
package_path="${output_dir}/spotlit_${debian_version}_amd64.deb"
dpkg-deb --root-owner-group --build "${package_root}" "${package_path}"
echo "Debian package: ${package_path}"

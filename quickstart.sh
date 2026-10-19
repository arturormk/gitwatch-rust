#!/usr/bin/env bash

set -Eeuo pipefail

readonly SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly BINARY_NAME="gitwatch"
readonly PACKAGE_NAME="gitwatch"
temporary_directory=""

cleanup() {
    if [[ -n "$temporary_directory" && -d "$temporary_directory" ]]; then
        rm -rf -- "$temporary_directory"
    fi
}

trap cleanup EXIT

die() {
    printf 'quickstart: %s\n' "$*" >&2
    exit 1
}

command_required() {
    command -v "$1" >/dev/null 2>&1 || die "$2"
}

package_version() {
    awk '
        /^\[package\]$/ { in_package = 1; next }
        /^\[/ { in_package = 0 }
        in_package && /^version[[:space:]]*=/ {
            value = $0
            sub(/^[^=]*=[[:space:]]*"/, "", value)
            sub(/"[[:space:]]*$/, "", value)
            print value
            exit
        }
    ' "$SCRIPT_DIR/Cargo.toml"
}

choose_target() {
    if (($# == 1)); then
        case "$1" in
            exe | deb | rpm) printf '%s\n' "$1" ;;
            *) die "usage: ./quickstart.sh [exe|deb|rpm]" ;;
        esac
        return
    fi

    (($# == 0)) || die "usage: ./quickstart.sh [exe|deb|rpm]"
    [[ -t 0 ]] || die "no interactive terminal; pass exactly one of: exe, deb, rpm"

    printf '%s\n' \
        'What would you like to build?' \
        '  1) Release executable only' \
        '  2) Debian package (.deb)' \
        '  3) RPM package (.rpm)' >&2
    read -r -p 'Selection [1]: ' selection

    case "$selection" in
        "" | 1 | exe) printf 'exe\n' ;;
        2 | deb) printf 'deb\n' ;;
        3 | rpm) printf 'rpm\n' ;;
        *) die "invalid selection; choose exactly one of 1, 2, or 3" ;;
    esac
}

deb_architecture() {
    if command -v dpkg >/dev/null 2>&1; then
        dpkg --print-architecture
        return
    fi

    case "$(uname -m)" in
        x86_64) printf 'amd64\n' ;;
        aarch64) printf 'arm64\n' ;;
        armv7l | armv7) printf 'armhf\n' ;;
        i386 | i486 | i586 | i686) printf 'i386\n' ;;
        *) die "cannot map architecture $(uname -m) to a Debian architecture" ;;
    esac
}

build_deb() {
    local version architecture work_dir package_root output installed_size
    version="$(package_version)"
    [[ -n "$version" ]] || die "could not read the package version from Cargo.toml"
    architecture="$(deb_architecture)"
    work_dir="$(mktemp -d -t gitwatch-deb.XXXXXXXX)"
    temporary_directory="$work_dir"
    package_root="$work_dir/$PACKAGE_NAME"
    output="$SCRIPT_DIR/target/packages/${PACKAGE_NAME}_${version}_${architecture}.deb"

    install -Dm755 "$SCRIPT_DIR/target/release/$BINARY_NAME" \
        "$package_root/usr/bin/$BINARY_NAME"
    install -Dm644 "$SCRIPT_DIR/LICENSE" \
        "$package_root/usr/share/doc/$PACKAGE_NAME/copyright"
    install -Dm644 "$SCRIPT_DIR/README.md" \
        "$package_root/usr/share/doc/$PACKAGE_NAME/README.md"
    installed_size="$(du -sk "$package_root/usr" | awk '{print $1}')"
    install -d "$package_root/DEBIAN" "$SCRIPT_DIR/target/packages"

    cat >"$package_root/DEBIAN/control" <<EOF
Package: $PACKAGE_NAME
Version: $version
Section: utils
Priority: optional
Architecture: $architecture
Installed-Size: $installed_size
Maintainer: gitwatch contributors
Description: Live, read-only terminal viewer for Git working-tree changes
 gitwatch displays uncommitted changes while another process edits a Git
 working tree.
EOF

    dpkg-deb --build --root-owner-group "$package_root" "$output"
    rm -rf -- "$work_dir"
    temporary_directory=""
    printf 'Built Debian package: %s\n' "$output"
}

build_rpm() {
    local version work_dir top_dir spec_file
    version="$(package_version)"
    [[ -n "$version" ]] || die "could not read the package version from Cargo.toml"
    work_dir="$(mktemp -d -t gitwatch-rpm.XXXXXXXX)"
    temporary_directory="$work_dir"
    top_dir="$work_dir/rpmbuild"
    spec_file="$top_dir/SPECS/$PACKAGE_NAME.spec"

    install -d "$top_dir"/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS} \
        "$SCRIPT_DIR/target/packages"
    install -m755 "$SCRIPT_DIR/target/release/$BINARY_NAME" \
        "$top_dir/SOURCES/$BINARY_NAME"
    install -m644 "$SCRIPT_DIR/LICENSE" "$top_dir/SOURCES/LICENSE"
    install -m644 "$SCRIPT_DIR/README.md" "$top_dir/SOURCES/README.md"

    cat >"$spec_file" <<EOF
Name:           $PACKAGE_NAME
Version:        $version
Release:        1%{?dist}
Summary:        Live, read-only terminal viewer for Git working-tree changes
License:        MIT
URL:            https://github.com/arturo/gitwatch-rust
Source0:        $BINARY_NAME
Source1:        LICENSE
Source2:        README.md

%global debug_package %{nil}

%description
gitwatch displays uncommitted changes while another process edits a Git
working tree.

%prep

%build

%install
install -Dm755 %{SOURCE0} %{buildroot}%{_bindir}/$BINARY_NAME
install -Dm644 %{SOURCE1} %{buildroot}%{_licensedir}/$PACKAGE_NAME/LICENSE
install -Dm644 %{SOURCE2} %{buildroot}%{_docdir}/$PACKAGE_NAME/README.md

%files
%{_bindir}/$BINARY_NAME
%{_licensedir}/$PACKAGE_NAME/LICENSE
%{_docdir}/$PACKAGE_NAME/README.md

%changelog
* Mon Oct 19 2026 gitwatch contributors - $version-1
- Initial package
EOF

    rpmbuild -bb --define "_topdir $top_dir" "$spec_file"
    find "$top_dir/RPMS" -type f -name '*.rpm' -exec cp -- {} "$SCRIPT_DIR/target/packages/" \;
    rm -rf -- "$work_dir"
    temporary_directory=""
    printf 'Built RPM package(s) in: %s\n' "$SCRIPT_DIR/target/packages"
}

main() {
    command_required cargo "cargo is required (install the stable Rust toolchain)"

    local target
    target="$(choose_target "$@")"

    case "$target" in
        deb)
            command_required dpkg-deb \
                "dpkg-deb is required for .deb packaging (install the dpkg package)"
            ;;
        rpm)
            command_required rpmbuild \
                "rpmbuild is required for .rpm packaging (install rpm-build or rpmdevtools)"
            ;;
    esac

    printf 'Building release executable...\n'
    cargo build --release --locked --manifest-path "$SCRIPT_DIR/Cargo.toml"
    printf 'Built executable: %s\n' "$SCRIPT_DIR/target/release/$BINARY_NAME"

    case "$target" in
        exe) ;;
        deb) build_deb ;;
        rpm) build_rpm ;;
    esac
}

main "$@"

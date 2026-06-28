# Spec file for Fedora COPR
#
# This builds the Go re-implementation (teleddns-go) but installs it under the
# original name `teleddns` as a drop-in replacement for the Rust package: same
# binary path, same systemd unit, same config path. See PRD.md, Milestone 4.
#
# To build locally:
#   dnf install golang systemd-rpm-macros
#   rpmbuild -tb teleddns-VERSION.tar.gz
#
# COPR builds from this spec file with network access enabled, so the Go
# toolchain auto-download (GOTOOLCHAIN=auto) and module proxy fetches work.

Name:           teleddns
Version:        0.3.0
Release:        1%{?dist}
Summary:        Advanced DDNS client with Netlink support

License:        GPL-3.0-or-later
URL:            https://github.com/tmshlvck/teleddns
Source0:        https://github.com/tmshlvck/teleddns/archive/refs/tags/v%{version}.tar.gz

# The go.mod toolchain directive may be newer than the distro's golang; with
# network access COPR lets the Go toolchain fetch the required version. Require
# a baseline that understands GOTOOLCHAIN (Go >= 1.21).
BuildRequires:  golang >= 1.21

# For systemd macros
BuildRequires:  systemd-rpm-macros

Requires:       systemd

# COPR builds these two arches natively.
ExclusiveArch:  x86_64 aarch64

%description
TeleDDNS is an advanced DDNS client with daemonization support (as a systemd
service) or one-shot running capability. When running in daemon mode, it
listens for Netlink messages and pools updates to minimize both DDNS
convergence time and resource usage.

Features:
- Efficient address change detection via Netlink
- Support for both IPv4 and IPv6
- Configurable update hooks
- Systemd integration

%prep
%autosetup -n %{name}-%{version}

%build
# Let the Go toolchain fetch the version required by go.mod if the distro's
# golang is older. COPR builds with network access enabled.
export GOTOOLCHAIN=auto
export GOFLAGS=-trimpath
export CGO_ENABLED=0
export GOCACHE=%{_builddir}/.gocache
export GOMODCACHE=%{_builddir}/.gomodcache
go build -ldflags "-X main.version=%{version}" \
    -o teleddns ./cmd/teleddns

# Compress man page
gzip -9 -k teleddns.1

%install
# Install binary
install -D -m 755 teleddns %{buildroot}%{_bindir}/teleddns

# Install systemd service
install -D -m 644 teleddns.service %{buildroot}%{_unitdir}/teleddns.service

# Install man page
install -D -m 644 teleddns.1.gz %{buildroot}%{_mandir}/man1/teleddns.1.gz

# Install documentation
install -D -m 644 README.md %{buildroot}%{_docdir}/%{name}/README.md
install -D -m 644 teleddns.yaml.sample %{buildroot}%{_docdir}/%{name}/teleddns.yaml.sample

# Create config directory
install -d %{buildroot}%{_sysconfdir}/teleddns

# Create state directory (persisted push state lives here; see PRD.md M3)
install -d -m 750 %{buildroot}%{_sharedstatedir}/teleddns

%post
%systemd_post teleddns.service
if [ ! -f %{_sysconfdir}/teleddns/teleddns.yaml ]; then
    echo "TeleDDNS installed. To configure:"
    echo "  cp %{_docdir}/%{name}/teleddns.yaml.sample %{_sysconfdir}/teleddns/teleddns.yaml"
    echo "  # Edit %{_sysconfdir}/teleddns/teleddns.yaml with your DDNS credentials"
    echo "  systemctl enable --now teleddns"
fi

%preun
%systemd_preun teleddns.service

%postun
%systemd_postun_with_restart teleddns.service

%files
%license LICENSE
%doc README.md
%doc %{_docdir}/%{name}/teleddns.yaml.sample
%{_bindir}/teleddns
%{_unitdir}/teleddns.service
%{_mandir}/man1/teleddns.1.gz
%dir %{_sysconfdir}/teleddns
%dir %attr(0750,root,root) %{_sharedstatedir}/teleddns

%changelog
* Sun Jun 28 2026 Tomas Hlavacek <tmshlvck@gmail.com> - 0.3.0-1
- Port to Go (teleddns-go); drop-in replacement for the Rust package
- Build a static Go binary (CGO disabled); no cargo/OpenSSL dependency
- Own /var/lib/teleddns for the persisted push state

* Mon Dec 15 2025 Tomas Hlavacek <tmshlvck@gmail.com> - 0.2.0-1
- Major version bump to v0.2.0
- CI/CD improvements: GitHub Actions pipeline fixes, COPR build enhancements
- Packaging: deb builds for multiple architectures (amd64, arm64, armhf, riscv64)
- Build system: cross-compilation fixes, vendor tarball improvements
- Debian/Ubuntu repository integration

* Mon Dec 15 2025 Tomas Hlavacek <tmshlvck@gmail.com> - 0.1.14-1
- Version bump

* Sun Dec 15 2025 Tomas Hlavacek <tmshlvck@gmail.com> - 0.1.13-1
- Simplify packaging: use cargo-deb for .deb, COPR with network for RPM
- Drop PPA support, use cross-compiled .deb binaries instead
- Build .deb for amd64, arm64, armhf, riscv64

* Sun Dec 15 2025 Tomas Hlavacek <tmshlvck@gmail.com> - 0.1.12-1
- Attempted PPA packaging fixes (superseded)

* Sat Dec 13 2025 Tomas Hlavacek <tmshlvck@gmail.com> - 0.1.11-1
- Suppress netlink-packet-route kernel compatibility warnings unless debug mode

* Sat Dec 13 2025 Tomas Hlavacek <tmshlvck@gmail.com> - 0.1.10-1
- Add RPM spec and Debian packaging
- Add Packit integration for automated COPR builds

* Fri Dec 05 2025 Tomas Hlavacek <tmshlvck@gmail.com> - 0.1.9-1
- Handle deprecated IPv6 addresses properly
- Align versions to v0.1.8

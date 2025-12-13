# Spec file for Fedora COPR
# This spec builds teleddns from vendored source tarball (offline build)
#
# To build locally:
#   dnf install rust cargo openssl-devel
#   rpmbuild -ta teleddns-VERSION-vendor.tar.gz
#
# For COPR, Packit downloads the vendored tarball from GitHub Releases.

Name:           teleddns
Version:        0.1.11
Release:        1%{?dist}
Summary:        Advanced DDNS client with Netlink support

License:        GPL-3.0-or-later
URL:            https://github.com/tmshlvck/teleddns
Source0:        https://github.com/tmshlvck/teleddns/releases/download/v%{version}/%{name}-%{version}.tar.gz

BuildRequires:  rust >= 1.70
BuildRequires:  cargo
BuildRequires:  openssl-devel
BuildRequires:  gcc

# For systemd macros
BuildRequires:  systemd-rpm-macros

Requires:       systemd

# Rust packages are not portable across architectures
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
# Build in release mode using vendored dependencies (offline)
export CARGO_HOME=$(pwd)/.cargo
cargo build --release --offline %{?_smp_mflags}

# Compress man page
gzip -9 -k teleddns.1

%install
# Install binary
install -D -m 755 target/release/teleddns %{buildroot}%{_bindir}/teleddns

# Install systemd service
install -D -m 644 teleddns.service %{buildroot}%{_unitdir}/teleddns.service

# Install man page
install -D -m 644 teleddns.1.gz %{buildroot}%{_mandir}/man1/teleddns.1.gz

# Install documentation
install -D -m 644 README.md %{buildroot}%{_docdir}/%{name}/README.md
install -D -m 644 teleddns.yaml.sample %{buildroot}%{_docdir}/%{name}/teleddns.yaml.sample

# Create config directory
install -d %{buildroot}%{_sysconfdir}/teleddns

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

%changelog
* Sat Dec 13 2025 Tomas Hlavacek <tmshlvck@gmail.com> - 0.1.11-1
- Suppress netlink-packet-route kernel compatibility warnings unless debug mode

* Sat Dec 13 2025 Tomas Hlavacek <tmshlvck@gmail.com> - 0.1.10-1
- Add RPM spec and Debian packaging
- Add Packit integration for automated COPR builds

* Fri Dec 05 2025 Tomas Hlavacek <tmshlvck@gmail.com> - 0.1.9-1
- Handle deprecated IPv6 addresses properly
- Align versions to v0.1.8


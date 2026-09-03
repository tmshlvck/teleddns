# Spec file for Fedora COPR
#
# To build locally:
#   dnf install rust cargo gcc
#   rpmbuild -tb teleddns-VERSION.tar.gz
#
# COPR builds from this spec file with network access enabled.

Name:           teleddns
Version:        0.4.2
Release:        1%{?dist}
Summary:        Advanced DDNS client with Netlink support

License:        GPL-3.0-or-later
URL:            https://github.com/tmshlvck/teleddns
Source0:        https://github.com/tmshlvck/teleddns/archive/refs/tags/v%{version}.tar.gz

BuildRequires:  rust >= 1.77
BuildRequires:  cargo
# gcc is required: the rustls stack pulls in the `ring` crate, which compiles C.
# (TLS is rustls, not OpenSSL — there is no openssl-devel dependency.)
BuildRequires:  gcc

# For systemd macros
BuildRequires:  systemd-rpm-macros

Requires:       systemd

# Fedora has no 32-bit ARM chroot, so armv7hf RPMs are not possible; armhf users
# are served by the .deb or the static musl binary from the GitHub release.
ExclusiveArch:  x86_64 aarch64 riscv64

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
cargo build --release %{?_smp_mflags}

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
* Thu Sep 03 2026 Tomas Hlavacek <tmshlvck@gmail.com> - 0.4.2-1
- No functional change to the client. Dependency, packaging and CI refresh.
- Netlink stack updated (rtnetlink 0.18 -> 0.23 and the netlink-packet crates)
  with no behaviour change, verified against a live kernel. Unused dependencies
  dropped and the unmaintained serde_yaml replaced with serde_yaml_ng. The
  minimum supported Rust is now 1.88.
- Static binaries (GitHub release) are built with cargo-zigbuild instead of
  cross: this restores the riscv64 static binary, now genuinely static, and
  adds an armel one for the original Raspberry Pi 1 / Zero. Does not affect
  this RPM.
- New armel .deb for Debian trixie, for the same ARMv6 devices.

* Thu Aug 13 2026 Tomas Hlavacek <tmshlvck@gmail.com> - 0.4.1-1
- Release pipeline fixes only; no functional change to the client since 0.4.0.
- The 0.4.0 tag published its .deb packages, but the COPR and crates.io steps
  failed on credential handling, so this is the first 0.4.x RPM.
- riscv64 static binaries are no longer published: the cross-compilation
  toolchain has no released riscv64 musl image. riscv64 is served by this RPM
  and by the .deb.

* Thu Aug 13 2026 Tomas Hlavacek <tmshlvck@gmail.com> - 0.4.0-1
- First release of the Rust rewrite; the changelog starts here, earlier entries
  described the Go implementation and do not apply to this package.
- dyndns2: IPv6 is sent as myipv6, the response body keyword is parsed and
  classified, and a stable User-Agent is sent. Permanent failures are no longer
  retried.
- Update timing reworked around the server's rate limit: debounce, exponential
  back-off, a client-side hourly budget per address family, and a periodic
  resync. Timing is configurable; every key is optional.
- Unknown configuration keys are refused instead of silently ignored, and a bad
  config reports an error rather than panicking.
- --oneshot always publishes and exits non-zero when the update fails.
- Graceful shutdown on SIGTERM and SIGINT as well as SIGHUP.
- riscv64 RPMs are built; Fedora has no 32-bit ARM target, so there is no
  armv7hf RPM.

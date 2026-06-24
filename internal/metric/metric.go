// Package metric assigns a preference score to each candidate IP address.
// Higher wins. It is a faithful port of the Rust client's compute_v4_metric /
// compute_v6_metric, including the exact base constants and bonuses, so the Go
// client selects the same address as the Rust client given the same inputs.
package metric

import (
	"net/netip"

	"golang.org/x/sys/unix"
)

// Base scores (matching the Rust constants):
//   - 0   unusable (loopback, link-local, documentation, multicast, ...)
//   - 1   "use only if nothing else": deprecated, or ULA/private when opted in
//   - 128 ordinary global address
//   - 129 EUI-64 global address (wins the tie against 128 so a stable,
//     interface-derived v6 is preferred over privacy-extension addresses)
//
// iface and flag bonuses are then added on top.
const (
	scoreUnusable    = 0
	scoreLastResort  = 1
	scoreGlobal      = 128
	scoreGlobalEUI64 = 129
)

// IPv6 ranges the Rust client forces to metric 0. netip has no helpers for
// these, so we match against explicit prefixes.
var (
	v6Documentation1 = netip.MustParsePrefix("2001:db8::/32")   // RFC 3849
	v6Documentation2 = netip.MustParsePrefix("3fff::/20")       // RFC 9637
	v6Benchmarking   = netip.MustParsePrefix("2001:2::/48")     // RFC 5180
	v4Documentation1 = netip.MustParsePrefix("192.0.2.0/24")    // TEST-NET-1
	v4Documentation2 = netip.MustParsePrefix("198.51.100.0/24") // TEST-NET-2
	v4Documentation3 = netip.MustParsePrefix("203.0.113.0/24")  // TEST-NET-3
)

// ifaceBonus mirrors the Rust iface_bonus: wired interfaces are preferred over
// wireless, both over anything else.
func ifaceBonus(iface string) uint8 {
	switch {
	case len(iface) >= 2 && iface[:2] == "en":
		return 16
	case len(iface) >= 2 && iface[:2] == "wl":
		return 8
	default:
		return 0
	}
}

// flagBonus mirrors the Rust flag_bonus: a permanent address gets +1. Note (as
// the Rust source warns) this means e.g. an OpenVPN tunnel's permanent address
// edges out a real interface's SLAAC address of the same class.
func flagBonus(flags int) uint8 {
	if flags&unix.IFA_F_PERMANENT != 0 {
		return 1
	}
	return 0
}

// V6 scores an IPv6 address. acceptULA enables fc00::/7 addresses (otherwise
// they score 0). The check order matches the Rust client exactly.
func V6(addr netip.Addr, iface string, flags int, acceptULA bool) uint8 {
	switch {
	case addr.IsLinkLocalUnicast() || addr.IsMulticast():
		return scoreUnusable
	case addr.Is4In6():
		return scoreUnusable
	case addr.IsLoopback() || addr.IsUnspecified():
		return scoreUnusable
	case v6Documentation1.Contains(addr) || v6Documentation2.Contains(addr):
		return scoreUnusable
	case v6Benchmarking.Contains(addr):
		return scoreUnusable
	case addr.IsPrivate(): // ULA, fc00::/7 (RFC 4193)
		if acceptULA {
			return scoreLastResort + ifaceBonus(iface) + flagBonus(flags)
		}
		return scoreUnusable
	case flags&unix.IFA_F_DEPRECATED != 0:
		return scoreLastResort + ifaceBonus(iface) + flagBonus(flags)
	case isEUI64(addr):
		return scoreGlobalEUI64 + ifaceBonus(iface) + flagBonus(flags)
	default:
		return scoreGlobal + ifaceBonus(iface) + flagBonus(flags)
	}
}

// V4 scores an IPv4 address. acceptPrivate enables RFC1918 addresses (otherwise
// they score 0). The check order matches the Rust client exactly.
func V4(addr netip.Addr, iface string, flags int, acceptPrivate bool) uint8 {
	switch {
	case addr.IsLoopback() || addr.IsUnspecified():
		return scoreUnusable
	case v4Documentation1.Contains(addr) || v4Documentation2.Contains(addr) || v4Documentation3.Contains(addr):
		return scoreUnusable
	case addr.IsLinkLocalUnicast(): // 169.254.0.0/16
		return scoreUnusable
	case addr.IsPrivate(): // RFC 1918
		if acceptPrivate {
			return scoreLastResort + ifaceBonus(iface) + flagBonus(flags)
		}
		return scoreUnusable
	default:
		return scoreGlobal + ifaceBonus(iface) + flagBonus(flags)
	}
}

// isEUI64 reports whether a v6 address was formed from a MAC via EUI-64, i.e.
// the interface identifier contains the inserted ff:fe in the middle (bytes 11
// and 12 are 0xff 0xfe). This mirrors the Rust octet check.
func isEUI64(addr netip.Addr) bool {
	b := addr.As16()
	return b[11] == 0xff && b[12] == 0xfe
}

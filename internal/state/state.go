// Package state builds and queries the set of reportable interface addresses.
//
// It mirrors the Rust client's IfaceIpAddr -> IfaceIpAddrData map: keyed by
// (address, prefix length), valued by the owning interface, the masked subnet,
// the computed metric, and the raw address flags. Milestone 2 rebuilds the
// whole map from a fresh netlink dump on every change (no incremental updates;
// that is Milestone 3).
package state

import (
	"fmt"
	"net"
	"net/netip"
	"sort"
	"strings"

	"github.com/tmshlvck/teleddns-go/internal/config"
	"github.com/tmshlvck/teleddns-go/internal/filter"
	"github.com/tmshlvck/teleddns-go/internal/metric"
	"github.com/vishvananda/netlink"
	"golang.org/x/sys/unix"
)

// AddrInput is one address resolved to its (up) interface name, ready for
// filtering and metric computation. It is the netlink-independent input to
// Build so the mapping logic can be unit-tested without a live socket.
type AddrInput struct {
	Iface     string
	Addr      netip.Addr
	PrefixLen int
	Flags     int
}

// AddrData is the per-address value stored in the map. It is a comparable
// struct so Map equality and change detection are trivial.
type AddrData struct {
	Iface  string
	Subnet netip.Prefix // network address of Addr, masked to PrefixLen
	Metric uint8
	Flags  int
}

// Map is the derived address table, keyed by the address and its prefix length.
type Map map[netip.Prefix]AddrData

// Build applies the interface filter and metric computation to the inputs and
// returns the derived map. Inputs should already be restricted to addresses on
// usable (up) interfaces, matching the Rust client which only resolves
// interface names for up links.
func Build(inputs []AddrInput, cfg *config.Config) Map {
	m := make(Map, len(inputs))
	for _, in := range inputs {
		if !filter.Match(in.Iface, cfg.Interfaces) {
			continue
		}
		var met uint8
		if in.Addr.Is4() {
			met = metric.V4(in.Addr, in.Iface, in.Flags, cfg.ReportIPv4Private)
		} else {
			met = metric.V6(in.Addr, in.Iface, in.Flags, cfg.ReportIPv6ULA)
		}
		key := netip.PrefixFrom(in.Addr, in.PrefixLen)
		m[key] = AddrData{
			Iface:  in.Iface,
			Subnet: key.Masked(),
			Metric: met,
			Flags:  in.Flags,
		}
	}
	return m
}

// Dump performs a fresh netlink dump of links and addresses, restricts to
// addresses on usable interfaces, and returns the derived map.
func Dump(cfg *config.Config) (Map, error) {
	links, err := netlink.LinkList()
	if err != nil {
		return nil, fmt.Errorf("link dump: %w", err)
	}
	up := make(map[int]string, len(links))
	for _, l := range links {
		if linkUp(l) {
			up[l.Attrs().Index] = l.Attrs().Name
		}
	}

	addrs, err := netlink.AddrList(nil, netlink.FAMILY_ALL)
	if err != nil {
		return nil, fmt.Errorf("address dump: %w", err)
	}
	inputs := make([]AddrInput, 0, len(addrs))
	for _, a := range addrs {
		if a.IPNet == nil {
			continue
		}
		iface, ok := up[a.LinkIndex]
		if !ok {
			continue // address on a down or unknown interface; ignored
		}
		addr, ok := netip.AddrFromSlice(a.IPNet.IP)
		if !ok {
			continue
		}
		prefixLen, _ := a.IPNet.Mask.Size()
		inputs = append(inputs, AddrInput{
			Iface:     iface,
			Addr:      addr.Unmap(),
			PrefixLen: prefixLen,
			Flags:     a.Flags,
		})
	}
	return Build(inputs, cfg), nil
}

// SelectBest returns the highest-metric IPv6 and IPv4 address, gated by
// enable_ipv6 / enable_ipv4. Addresses with metric 0 are never selected. Ties
// are broken deterministically by choosing the numerically smaller address
// (the Rust client leaves ties to nondeterministic map iteration; sorting here
// makes the choice stable and reproducible). An unset result is the zero
// netip.Addr, for which IsValid reports false.
func (m Map) SelectBest(cfg *config.Config) (best6, best4 netip.Addr) {
	var m6, m4 uint8
	for key, d := range m {
		if d.Metric == 0 {
			continue
		}
		addr := key.Addr()
		if addr.Is4() {
			if cfg.EnableIPv4 && (d.Metric > m4 || (d.Metric == m4 && best4.IsValid() && addr.Less(best4))) {
				best4, m4 = addr, d.Metric
			}
		} else {
			if cfg.EnableIPv6 && (d.Metric > m6 || (d.Metric == m6 && best6.IsValid() && addr.Less(best6))) {
				best6, m6 = addr, d.Metric
			}
		}
	}
	return best6, best4
}

// Knows reports whether the map already contains exactly this address (same
// address, prefix length, and flags). A NEWADDR re-announcement that Knows
// returns true for cannot change the selection or the derived map, so it can be
// ignored without rebuilding state — this is the event-level dampening the Rust
// client performs against its iface_addrs_map. A flag change (e.g. an address
// becoming IFA_F_DEPRECATED) makes Knows return false so the change is acted on.
func (m Map) Knows(addr netip.Addr, prefixLen, flags int) bool {
	d, ok := m[netip.PrefixFrom(addr, prefixLen)]
	return ok && d.Flags == flags
}

// Equal reports whether two maps have identical keys and values.
func (m Map) Equal(other Map) bool {
	if len(m) != len(other) {
		return false
	}
	for k, v := range m {
		if ov, ok := other[k]; !ok || ov != v {
			return false
		}
	}
	return true
}

// Summary renders the map as a stable, sorted one-line string for logging:
// "<addr>/<pfx> iface=<n> metric=<m>; ...".
func (m Map) Summary() string {
	if len(m) == 0 {
		return "(no matching addresses)"
	}
	keys := make([]netip.Prefix, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	sort.Slice(keys, func(i, j int) bool { return keys[i].Addr().Less(keys[j].Addr()) })
	parts := make([]string, 0, len(keys))
	for _, k := range keys {
		d := m[k]
		parts = append(parts, fmt.Sprintf("%s iface=%s metric=%d", k, d.Iface, d.Metric))
	}
	return strings.Join(parts, "; ")
}

// linkUp mirrors the Rust client's notion of a usable interface: up at every
// layer (UP + LOWER_UP + RUNNING) or reported operationally up.
func linkUp(l netlink.Link) bool {
	a := l.Attrs()
	if a.Flags&net.FlagUp != 0 && a.RawFlags&unix.IFF_LOWER_UP != 0 && a.RawFlags&unix.IFF_RUNNING != 0 {
		return true
	}
	return a.OperState == netlink.OperUp
}

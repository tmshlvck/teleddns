package watch

import (
	"fmt"
	"net"

	"github.com/vishvananda/netlink"
	"golang.org/x/sys/unix"
)

// linkStateFrom decodes a netlink.Link into our LinkState.
func linkStateFrom(l netlink.Link) LinkState {
	a := l.Attrs()
	return LinkState{
		Index:     a.Index,
		Name:      a.Name,
		Up:        a.Flags&net.FlagUp != 0,
		LowerUp:   a.RawFlags&unix.IFF_LOWER_UP != 0,
		Running:   a.RawFlags&unix.IFF_RUNNING != 0,
		OperState: a.OperState.String(),
		MTU:       a.MTU,
	}
}

// family returns the rtnetlink-style family label for an IP.
func family(ip net.IP) string {
	if ip.To4() != nil {
		return "inet"
	}
	return "inet6"
}

// scopeName maps an rtnetlink address scope to its conventional `ip` label.
func scopeName(scope int) string {
	switch scope {
	case unix.RT_SCOPE_UNIVERSE:
		return "global"
	case unix.RT_SCOPE_SITE:
		return "site"
	case unix.RT_SCOPE_LINK:
		return "link"
	case unix.RT_SCOPE_HOST:
		return "host"
	case unix.RT_SCOPE_NOWHERE:
		return "nowhere"
	default:
		return fmt.Sprintf("scope(%d)", scope)
	}
}

// addrFlagBits lists every IFA_F_* bit in a stable order for decoding.
var addrFlagBits = []struct {
	bit  int
	name string
}{
	{unix.IFA_F_SECONDARY, "secondary"}, // == IFA_F_TEMPORARY for IPv6
	{unix.IFA_F_NODAD, "nodad"},
	{unix.IFA_F_OPTIMISTIC, "optimistic"},
	{unix.IFA_F_DADFAILED, "dadfailed"},
	{unix.IFA_F_HOMEADDRESS, "homeaddress"},
	{unix.IFA_F_DEPRECATED, "deprecated"},
	{unix.IFA_F_TENTATIVE, "tentative"},
	{unix.IFA_F_PERMANENT, "permanent"},
	{unix.IFA_F_MANAGETEMPADDR, "mngtmpaddr"},
	{unix.IFA_F_NOPREFIXROUTE, "noprefixroute"},
	{unix.IFA_F_MCAUTOJOIN, "mcautojoin"},
	{unix.IFA_F_STABLE_PRIVACY, "stableprivacy"},
}

// decodeAddrFlags expands an IFA_F_* bitmask into human-readable names.
func decodeAddrFlags(flags int) []string {
	var out []string
	for _, f := range addrFlagBits {
		if flags&f.bit != 0 {
			out = append(out, f.name)
		}
	}
	return out
}

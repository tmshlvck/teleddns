// Package watch turns rtnetlink link/address activity into a typed event
// stream. It wraps github.com/vishvananda/netlink: LinkList/AddrList for the
// startup dump, LinkSubscribe/AddrSubscribe for live multicast notifications.
package watch

import "net"

// EventKind identifies which RTM_* message a netlink event corresponds to.
type EventKind string

const (
	EventNewLink EventKind = "NEWLINK" // RTM_NEWLINK: interface added or changed
	EventDelLink EventKind = "DELLINK" // RTM_DELLINK: interface removed
	EventNewAddr EventKind = "NEWADDR" // RTM_NEWADDR: address added or changed
	EventDelAddr EventKind = "DELADDR" // RTM_DELADDR: address removed
)

// LinkState is the decoded subset of an interface's attributes that the DDNS
// client cares about: identity, the three "is it usable" flags, and operstate.
type LinkState struct {
	Index     int
	Name      string
	Up        bool // IFF_UP — administratively enabled
	LowerUp   bool // IFF_LOWER_UP — physical/carrier link present
	Running   bool // IFF_RUNNING — operational
	OperState string
	MTU       int
}

// Usable reports whether the link is up at every layer the Rust client checks.
func (l LinkState) Usable() bool { return l.Up && l.LowerUp && l.Running }

// AddrState is the decoded subset of an address's attributes. Name is the
// resolved interface name, or "" if the ifindex was not known at decode time.
type AddrState struct {
	Index     int
	Name      string
	IP        net.IP
	PrefixLen int
	Family    string   // "inet" or "inet6"
	Scope     string   // "global", "link", "host", ...
	Flags     []string // decoded IFA_F_* names, e.g. ["permanent"]
}

// Event is one observed netlink change. Exactly one of Link or Addr is set,
// determined by Kind. PrevLink is non-nil only for an EventNewLink that
// updates an already-known interface; it carries the prior LinkState so a
// consumer can report exactly what changed (operstate, flags, MTU).
type Event struct {
	Kind     EventKind
	Initial  bool // produced by the startup dump rather than a live message
	Link     *LinkState
	PrevLink *LinkState
	Addr     *AddrState
}

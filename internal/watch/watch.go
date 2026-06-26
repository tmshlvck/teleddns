package watch

import (
	"fmt"
	"net"

	"github.com/vishvananda/netlink"
	"golang.org/x/sys/unix"
)

// Watcher produces a typed stream of netlink events. It keeps a table of known
// links so address events can be tagged with an interface name and so a
// NEWLINK for an already-seen interface can be reported as a change (with the
// prior state) rather than as a brand-new interface.
//
// A Watcher is not safe for concurrent use: Dump runs on the caller's
// goroutine before Subscribe, and the Subscribe goroutine is the sole owner of
// the links table thereafter.
type Watcher struct {
	links map[int]LinkState
}

// New returns an empty Watcher.
func New() *Watcher {
	return &Watcher{links: make(map[int]LinkState)}
}

// Dump enumerates all current links and addresses and returns them as Events
// with Initial set. It also seeds the link table used to resolve interface
// names for later address events. This mirrors the Rust client's startup
// "full read + trigger first update" behavior.
func (w *Watcher) Dump() ([]Event, error) {
	links, err := netlink.LinkList()
	if err != nil {
		return nil, fmt.Errorf("link dump: %w", err)
	}
	events := make([]Event, 0, len(links))
	for _, l := range links {
		ls := linkStateFrom(l)
		w.links[ls.Index] = ls
		link := ls
		events = append(events, Event{Kind: EventNewLink, Initial: true, Link: &link})
	}

	addrs, err := netlink.AddrList(nil, netlink.FAMILY_ALL)
	if err != nil {
		return nil, fmt.Errorf("address dump: %w", err)
	}
	for _, a := range addrs {
		as := w.addrState(a.LinkIndex, *a.IPNet, a.Scope, a.Flags)
		addr := as
		events = append(events, Event{Kind: EventNewAddr, Initial: true, Addr: &addr})
	}
	return events, nil
}

// Subscribe opens the rtnetlink multicast subscriptions (RTNLGRP_LINK via
// LinkSubscribe, RTNLGRP_IPV4_IFADDR + RTNLGRP_IPV6_IFADDR via AddrSubscribe)
// and returns a channel of decoded events. Closing done tears down the
// subscriptions and the channel. Subscribe must be called after Dump.
func (w *Watcher) Subscribe(done <-chan struct{}) (<-chan Event, error) {
	linkCh := make(chan netlink.LinkUpdate, 64)
	if err := netlink.LinkSubscribe(linkCh, done); err != nil {
		return nil, fmt.Errorf("link subscribe: %w", err)
	}
	addrCh := make(chan netlink.AddrUpdate, 64)
	if err := netlink.AddrSubscribe(addrCh, done); err != nil {
		return nil, fmt.Errorf("address subscribe: %w", err)
	}

	out := make(chan Event, 64)
	go func() {
		defer close(out)
		for {
			var ev Event
			select {
			case <-done:
				return
			case lu, ok := <-linkCh:
				if !ok {
					return
				}
				ev = w.handleLink(lu)
			case au, ok := <-addrCh:
				if !ok {
					return
				}
				ev = w.handleAddr(au)
			}
			select {
			case out <- ev:
			case <-done:
				return
			}
		}
	}()
	return out, nil
}

// handleLink decodes a live link update and reconciles the link table.
func (w *Watcher) handleLink(lu netlink.LinkUpdate) Event {
	ls := linkStateFrom(lu.Link)
	if lu.Header.Type == unix.RTM_DELLINK {
		delete(w.links, ls.Index)
		link := ls
		return Event{Kind: EventDelLink, Link: &link}
	}
	// RTM_NEWLINK: a new interface or a change to an existing one.
	prev, existed := w.links[ls.Index]
	w.links[ls.Index] = ls
	link := ls
	ev := Event{Kind: EventNewLink, Link: &link}
	if existed {
		p := prev
		ev.PrevLink = &p
	}
	return ev
}

// handleAddr decodes a live address update.
func (w *Watcher) handleAddr(au netlink.AddrUpdate) Event {
	as := w.addrState(au.LinkIndex, au.LinkAddress, au.Scope, au.Flags)
	addr := as
	if au.NewAddr {
		return Event{Kind: EventNewAddr, Addr: &addr}
	}
	return Event{Kind: EventDelAddr, Addr: &addr}
}

// addrState builds an AddrState, resolving the interface name from the link
// table when possible.
func (w *Watcher) addrState(ifindex int, ipnet net.IPNet, scope, flags int) AddrState {
	name := ""
	if l, ok := w.links[ifindex]; ok {
		name = l.Name
	}
	prefixLen, _ := ipnet.Mask.Size()
	return AddrState{
		Index:     ifindex,
		Name:      name,
		IP:        ipnet.IP,
		PrefixLen: prefixLen,
		Family:    family(ipnet.IP),
		Scope:     scopeName(scope),
		Flags:     decodeAddrFlags(flags),
		FlagBits:  flags,
	}
}

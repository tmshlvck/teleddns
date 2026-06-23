// Package observe renders watch.Event values to the log. Milestone 1 has no
// filtering, selection, or DDNS push — printing is the whole job.
package observe

import (
	"fmt"
	"log/slog"
	"strings"

	"github.com/tmshlvck/teleddns-go/internal/watch"
)

// Print logs a single event: a one-line human summary as the message, plus the
// structured key=value fields the PRD asks for.
func Print(logger *slog.Logger, e watch.Event) {
	switch e.Kind {
	case watch.EventNewLink, watch.EventDelLink:
		printLink(logger, e)
	case watch.EventNewAddr, watch.EventDelAddr:
		printAddr(logger, e)
	default:
		logger.Warn("unknown event kind", "kind", string(e.Kind))
	}
}

func printLink(logger *slog.Logger, e watch.Event) {
	l := e.Link
	attrs := []any{
		"event", string(e.Kind),
		"initial", e.Initial,
		"ifindex", l.Index,
		"ifname", l.Name,
		"up", l.Up,
		"lowerup", l.LowerUp,
		"running", l.Running,
		"operstate", l.OperState,
		"mtu", l.MTU,
	}

	var summary string
	switch {
	case e.Kind == watch.EventDelLink:
		summary = fmt.Sprintf("link %s removed", l.Name)
	case e.Initial:
		summary = fmt.Sprintf("link %s present (%s)", l.Name, linkStatus(*l))
	case e.PrevLink == nil:
		summary = fmt.Sprintf("link %s added (%s)", l.Name, linkStatus(*l))
	default:
		changes := linkChanges(*e.PrevLink, *l)
		if len(changes) == 0 {
			summary = fmt.Sprintf("link %s notified, no tracked change", l.Name)
		} else {
			summary = fmt.Sprintf("link %s changed: %s", l.Name, strings.Join(changes, ", "))
			attrs = append(attrs, "changes", strings.Join(changes, ","))
		}
	}
	logger.Info(summary, attrs...)
}

func printAddr(logger *slog.Logger, e watch.Event) {
	a := e.Addr
	ifname := a.Name
	if ifname == "" {
		ifname = fmt.Sprintf("if%d", a.Index)
	}
	flags := strings.Join(a.Flags, ",")
	attrs := []any{
		"event", string(e.Kind),
		"initial", e.Initial,
		"ifindex", a.Index,
		"ifname", a.Name,
		"addr", a.IP.String(),
		"prefixlen", a.PrefixLen,
		"family", a.Family,
		"scope", a.Scope,
		"flags", flags,
	}

	verb := "added"
	if e.Kind == watch.EventDelAddr {
		verb = "removed"
	} else if e.Initial {
		verb = "present"
	}
	summary := fmt.Sprintf("addr %s/%d %s on %s [%s scope=%s flags=%s]",
		a.IP, a.PrefixLen, verb, ifname, a.Family, a.Scope, orNone(flags))
	logger.Info(summary, attrs...)
}

// linkStatus is a compact UP/DOWN word for a link.
func linkStatus(l watch.LinkState) string {
	if l.Usable() {
		return "up"
	}
	return "down"
}

// linkChanges lists the human-readable differences between two link states,
// restricted to the attributes the DDNS client tracks.
func linkChanges(prev, cur watch.LinkState) []string {
	var c []string
	if prev.Up != cur.Up {
		c = append(c, fmt.Sprintf("up %t->%t", prev.Up, cur.Up))
	}
	if prev.LowerUp != cur.LowerUp {
		c = append(c, fmt.Sprintf("lowerup %t->%t", prev.LowerUp, cur.LowerUp))
	}
	if prev.Running != cur.Running {
		c = append(c, fmt.Sprintf("running %t->%t", prev.Running, cur.Running))
	}
	if prev.OperState != cur.OperState {
		c = append(c, fmt.Sprintf("operstate %s->%s", prev.OperState, cur.OperState))
	}
	if prev.MTU != cur.MTU {
		c = append(c, fmt.Sprintf("mtu %d->%d", prev.MTU, cur.MTU))
	}
	if prev.Name != cur.Name {
		c = append(c, fmt.Sprintf("ifname %s->%s", prev.Name, cur.Name))
	}
	return c
}

func orNone(s string) string {
	if s == "" {
		return "-"
	}
	return s
}

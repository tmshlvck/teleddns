// Package filter implements the interface name matching used to decide which
// interfaces' addresses are eligible for DDNS reporting.
package filter

// Match reports whether ifname is accepted by the pattern list, matched
// left-to-right. This is a port of the Rust client's match_iface_pattern:
//
//   - a "-<name>" entry that matches ifname is an immediate reject,
//   - an exact "<name>" entry is an immediate accept,
//   - a "*" entry is an immediate accept,
//   - if nothing matches, the default is reject.
//
// Because matching stops at the first decision, ordering matters: e.g.
// ["-virbr0", "*"] accepts everything except virbr0, while ["*", "-virbr0"]
// accepts virbr0 too (the "*" wins first).
func Match(ifname string, patterns []string) bool {
	negative := "-" + ifname
	for _, p := range patterns {
		switch {
		case p == negative:
			return false
		case p == ifname:
			return true
		case p == "*":
			return true
		}
	}
	return false
}

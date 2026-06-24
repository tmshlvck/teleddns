// Package hooks runs the post-change actions configured under `hooks:`: writing
// nftables set definitions for the currently observed subnets, and running a
// shell command. Hooks fire on every observed state change, matching the Rust
// client.
package hooks

import (
	"context"
	"log/slog"
	"net/netip"
	"os"
	"os/exec"
	"sort"
	"strings"

	"github.com/tmshlvck/teleddns-go/internal/config"
	"github.com/tmshlvck/teleddns-go/internal/state"
)

// Apply runs every configured hook against the current address map.
func Apply(ctx context.Context, m state.Map, cfg *config.Config) {
	for _, h := range cfg.Hooks {
		if h.NftSetsOutfile != "" {
			if err := writeNftSets(h.NftSetsOutfile, m); err != nil {
				slog.Error("failed to write nft sets", "file", h.NftSetsOutfile, "err", err)
			}
		}
		if h.Shell != "" {
			runShell(ctx, h.Shell)
		}
	}
}

// RenderNftSets returns the nftables set definitions for the subnets in m:
// LOCAL_NET4 for IPv4 and LOCAL_NET6 for IPv6, each containing the deduplicated
// masked subnets. Entries are sorted so the output is stable across runs (the
// Rust client emits them in nondeterministic map order).
func RenderNftSets(m state.Map) string {
	var v4, v6 []netip.Prefix
	seen := make(map[netip.Prefix]struct{}, len(m))
	for _, d := range m {
		if _, dup := seen[d.Subnet]; dup {
			continue
		}
		seen[d.Subnet] = struct{}{}
		if d.Subnet.Addr().Is4() {
			v4 = append(v4, d.Subnet)
		} else {
			v6 = append(v6, d.Subnet)
		}
	}
	sortPrefixes(v4)
	sortPrefixes(v6)

	var b strings.Builder
	writeSet(&b, "LOCAL_NET4", v4)
	writeSet(&b, "LOCAL_NET6", v6)
	return b.String()
}

func writeSet(b *strings.Builder, name string, prefixes []netip.Prefix) {
	b.WriteString("define ")
	b.WriteString(name)
	b.WriteString("={\n")
	for i, p := range prefixes {
		if i > 0 {
			b.WriteString(",\n")
		}
		b.WriteString(p.String())
	}
	b.WriteString("\n}\n")
}

func sortPrefixes(ps []netip.Prefix) {
	sort.Slice(ps, func(i, j int) bool {
		if ps[i].Addr() == ps[j].Addr() {
			return ps[i].Bits() < ps[j].Bits()
		}
		return ps[i].Addr().Less(ps[j].Addr())
	})
}

func writeNftSets(path string, m state.Map) error {
	slog.Info("writing nft sets", "file", path)
	return os.WriteFile(path, []byte(RenderNftSets(m)), 0o644)
}

func runShell(ctx context.Context, cmd string) {
	slog.Info("running shell hook", "cmd", cmd)
	out, err := exec.CommandContext(ctx, "sh", "-c", cmd).CombinedOutput()
	if err != nil {
		slog.Warn("shell hook failed", "cmd", cmd, "err", err, "output", strings.TrimSpace(string(out)))
		return
	}
	slog.Info("shell hook succeeded", "cmd", cmd, "output", strings.TrimSpace(string(out)))
}

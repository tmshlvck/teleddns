package hooks

import (
	"net/netip"
	"testing"

	"github.com/tmshlvck/teleddns-go/internal/config"
	"github.com/tmshlvck/teleddns-go/internal/state"
)

func buildMap(addrs ...string) state.Map {
	c := &config.Config{EnableIPv4: true, EnableIPv6: true, Interfaces: []string{"*"}, ReportIPv4Private: true, ReportIPv6ULA: true}
	inputs := make([]state.AddrInput, 0, len(addrs))
	for _, a := range addrs {
		p := netip.MustParsePrefix(a)
		inputs = append(inputs, state.AddrInput{Iface: "eth0", Addr: p.Addr(), PrefixLen: p.Bits()})
	}
	return state.Build(inputs, c)
}

func TestRenderNftSetsSortedAndDeduped(t *testing.T) {
	// Two addresses in the same v4 subnet must collapse to one entry; v6 too.
	m := buildMap(
		"203.0.114.9/24",
		"203.0.114.5/24", // same subnet 203.0.114.0/24
		"198.18.0.7/24",  // different subnet, sorts before .114
		"2606:4700::2/64",
		"2606:4700::1/64", // same subnet 2606:4700::/64
	)
	got := RenderNftSets(m)
	want := "define LOCAL_NET4={\n" +
		"198.18.0.0/24,\n" +
		"203.0.114.0/24\n" +
		"}\n" +
		"define LOCAL_NET6={\n" +
		"2606:4700::/64\n" +
		"}\n"
	if got != want {
		t.Errorf("RenderNftSets mismatch:\n--- got ---\n%s\n--- want ---\n%s", got, want)
	}
}

func TestRenderNftSetsEmpty(t *testing.T) {
	got := RenderNftSets(state.Map{})
	want := "define LOCAL_NET4={\n\n}\ndefine LOCAL_NET6={\n\n}\n"
	if got != want {
		t.Errorf("empty RenderNftSets = %q, want %q", got, want)
	}
}

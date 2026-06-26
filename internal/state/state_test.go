package state

import (
	"net/netip"
	"testing"

	"github.com/tmshlvck/teleddns-go/internal/config"
	"golang.org/x/sys/unix"
)

func cfg() *config.Config {
	return &config.Config{
		EnableIPv6: true,
		EnableIPv4: true,
		Interfaces: []string{"*"},
	}
}

func in(iface, addr string, pfx, flags int) AddrInput {
	return AddrInput{Iface: iface, Addr: netip.MustParseAddr(addr), PrefixLen: pfx, Flags: flags}
}

func TestBuildFiltersAndScores(t *testing.T) {
	c := cfg()
	c.Interfaces = []string{"-virbr0", "*"}
	m := Build([]AddrInput{
		in("enp3s0", "2606:4700::1111", 64, unix.IFA_F_PERMANENT),
		in("virbr0", "2606:4700::2222", 64, 0), // filtered out
		in("enp3s0", "203.0.114.5", 24, 0),
	}, c)

	if len(m) != 2 {
		t.Fatalf("expected 2 entries (virbr0 filtered), got %d: %s", len(m), m.Summary())
	}
	v6 := netip.MustParsePrefix("2606:4700::1111/64")
	d, ok := m[v6]
	if !ok {
		t.Fatalf("missing v6 entry")
	}
	if d.Metric != 145 { // global(128) + en*(16) + permanent(1)
		t.Fatalf("unexpected v6 metric: %d, want 145", d.Metric)
	}
}

func TestSelectBest(t *testing.T) {
	c := cfg()
	m := Build([]AddrInput{
		in("eth0", "2606:4700::1111", 64, 0),                // global 128
		in("eth0", "2606:4700::0200:5eff:fe00:5301", 64, 0), // eui-64 129, should win
		in("eth0", "fe80::1", 64, 0),                        // link-local, metric 0
		in("eth0", "203.0.114.5", 24, 0),                    // v4 global 128
		in("eth0", "203.0.114.9", 24, 0),                    // v4 global 128, tie
	}, c)

	best6, best4 := m.SelectBest(c)
	if best6.String() != "2606:4700::200:5eff:fe00:5301" {
		t.Errorf("best6 = %s, want eui-64 address", best6)
	}
	// tie broken by smaller address
	if best4.String() != "203.0.114.5" {
		t.Errorf("best4 = %s, want 203.0.114.5 (smaller of the tie)", best4)
	}
}

func TestSelectBestGating(t *testing.T) {
	c := cfg()
	c.EnableIPv4 = false
	m := Build([]AddrInput{
		in("eth0", "2606:4700::1111", 64, 0),
		in("eth0", "203.0.114.5", 24, 0),
	}, c)
	best6, best4 := m.SelectBest(c)
	if !best6.IsValid() {
		t.Errorf("expected a v6 selection")
	}
	if best4.IsValid() {
		t.Errorf("v4 disabled, expected no v4 selection, got %s", best4)
	}
}

func TestSelectBestNoUsableAddrs(t *testing.T) {
	c := cfg()
	m := Build([]AddrInput{
		in("eth0", "fe80::1", 64, 0),     // link-local -> 0
		in("eth0", "169.254.1.1", 16, 0), // v4 link-local -> 0
	}, c)
	best6, best4 := m.SelectBest(c)
	if best6.IsValid() || best4.IsValid() {
		t.Errorf("expected no selection, got v6=%v v4=%v", best6, best4)
	}
}

func TestKnows(t *testing.T) {
	c := cfg()
	m := Build([]AddrInput{
		in("eth0", "2a02:169:c33e:1:1266:6aff:fe23:b678", 64, unix.IFA_F_MANAGETEMPADDR|unix.IFA_F_NOPREFIXROUTE),
		in("eth0", "192.168.1.229", 24, 0),
	}, c)

	v6 := netip.MustParseAddr("2a02:169:c33e:1:1266:6aff:fe23:b678")

	// Same address, same flags -> known (a SLAAC re-announcement).
	if !m.Knows(v6, 64, unix.IFA_F_MANAGETEMPADDR|unix.IFA_F_NOPREFIXROUTE) {
		t.Errorf("expected re-announced address with identical flags to be known")
	}
	// Same address, flags changed (now deprecated) -> not known, must act.
	if m.Knows(v6, 64, unix.IFA_F_MANAGETEMPADDR|unix.IFA_F_NOPREFIXROUTE|unix.IFA_F_DEPRECATED) {
		t.Errorf("flag change (DEPRECATED) should not be treated as known")
	}
	// Different prefix length -> not known.
	if m.Knows(v6, 128, unix.IFA_F_MANAGETEMPADDR|unix.IFA_F_NOPREFIXROUTE) {
		t.Errorf("different prefix length should not be known")
	}
	// Unknown address -> not known.
	if m.Knows(netip.MustParseAddr("2a02:169:c33e:1::dead"), 64, 0) {
		t.Errorf("unseen address should not be known")
	}
}

func TestEqual(t *testing.T) {
	c := cfg()
	a := Build([]AddrInput{in("eth0", "2606:4700::1111", 64, 0)}, c)
	b := Build([]AddrInput{in("eth0", "2606:4700::1111", 64, 0)}, c)
	if !a.Equal(b) {
		t.Errorf("identical maps should be equal")
	}
	d := Build([]AddrInput{in("eth0", "2606:4700::1111", 64, unix.IFA_F_PERMANENT)}, c)
	if a.Equal(d) {
		t.Errorf("flag difference should make maps unequal")
	}
}

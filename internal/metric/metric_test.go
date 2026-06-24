package metric

import (
	"net/netip"
	"testing"

	"golang.org/x/sys/unix"
)

func TestV6(t *testing.T) {
	const permanent = unix.IFA_F_PERMANENT
	const deprecated = unix.IFA_F_DEPRECATED

	tests := []struct {
		name      string
		addr      string
		iface     string
		flags     int
		acceptULA bool
		want      uint8
	}{
		{"global", "2606:4700::1111", "eth0", 0, false, 128},
		{"global on en* gets +16", "2606:4700::1111", "enp3s0", 0, false, 144},
		{"global on wl* gets +8", "2606:4700::1111", "wlan0", 0, false, 136},
		{"global permanent +1", "2606:4700::1111", "eth0", permanent, false, 129},
		{"global en* permanent", "2606:4700::1111", "enp3s0", permanent, false, 145},
		{"eui-64 beats plain global", "2606:4700::0200:5eff:fe00:5301", "eth0", 0, false, 129},
		{"eui-64 en* permanent", "2606:4700::0200:5eff:fe00:5301", "enp3s0", permanent, false, 146},
		{"link-local", "fe80::1", "eth0", 0, false, 0},
		{"loopback", "::1", "lo", 0, false, 0},
		{"unspecified", "::", "eth0", 0, false, 0},
		{"documentation 2001:db8", "2001:db8::1", "eth0", 0, false, 0},
		{"documentation 3fff", "3fff::1", "eth0", 0, false, 0},
		{"benchmarking 2001:2", "2001:2:0::1", "eth0", 0, false, 0},
		{"ipv4-mapped", "::ffff:192.0.2.1", "eth0", 0, false, 0},
		{"ula rejected by default", "fd00::1", "eth0", 0, false, 0},
		{"ula accepted is last resort", "fd00::1", "eth0", 0, true, 1},
		{"ula accepted en* permanent", "fd00::1", "enp3s0", permanent, true, 18},
		{"deprecated global demoted to last resort", "2606:4700::1111", "eth0", deprecated, false, 1},
		{"deprecated global en* keeps bonuses", "2606:4700::1111", "enp3s0", deprecated, false, 17},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			addr := netip.MustParseAddr(tt.addr)
			if got := V6(addr, tt.iface, tt.flags, tt.acceptULA); got != tt.want {
				t.Errorf("V6(%s, %q, %#x, %v) = %d, want %d", tt.addr, tt.iface, tt.flags, tt.acceptULA, got, tt.want)
			}
		})
	}
}

func TestV4(t *testing.T) {
	const permanent = unix.IFA_F_PERMANENT

	tests := []struct {
		name          string
		addr          string
		iface         string
		flags         int
		acceptPrivate bool
		want          uint8
	}{
		{"global", "203.0.114.5", "eth0", 0, false, 128},
		{"global en*", "203.0.114.5", "enp3s0", 0, false, 144},
		{"global permanent", "203.0.114.5", "eth0", permanent, false, 129},
		{"loopback", "127.0.0.1", "lo", 0, false, 0},
		{"unspecified", "0.0.0.0", "eth0", 0, false, 0},
		{"link-local", "169.254.1.1", "eth0", 0, false, 0},
		{"documentation test-net-1", "192.0.2.5", "eth0", 0, false, 0},
		{"documentation test-net-2", "198.51.100.5", "eth0", 0, false, 0},
		{"documentation test-net-3", "203.0.113.5", "eth0", 0, false, 0},
		{"private rejected by default", "192.168.1.5", "eth0", 0, false, 0},
		{"private accepted is last resort", "192.168.1.5", "eth0", 0, true, 1},
		{"private accepted en* permanent", "10.0.0.5", "enp3s0", permanent, true, 18},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			addr := netip.MustParseAddr(tt.addr)
			if got := V4(addr, tt.iface, tt.flags, tt.acceptPrivate); got != tt.want {
				t.Errorf("V4(%s, %q, %#x, %v) = %d, want %d", tt.addr, tt.iface, tt.flags, tt.acceptPrivate, got, tt.want)
			}
		})
	}
}

package filter

import "testing"

func TestMatch(t *testing.T) {
	tests := []struct {
		name     string
		ifname   string
		patterns []string
		want     bool
	}{
		{"wildcard accepts all", "eth0", []string{"*"}, true},
		{"exact accept", "eth0", []string{"eth0"}, true},
		{"no match default reject", "eth0", []string{"wlan0"}, false},
		{"empty list rejects", "eth0", nil, false},
		{"negative rejects", "virbr0", []string{"-virbr0", "*"}, false},
		{"negative before wildcard, other iface accepted", "eth0", []string{"-virbr0", "*"}, true},
		{"wildcard before negative wins (accept)", "virbr0", []string{"*", "-virbr0"}, true},
		{"exact before negative wins (accept)", "eth0", []string{"eth0", "-eth0"}, true},
		{"negative before exact wins (reject)", "eth0", []string{"-eth0", "eth0"}, false},
		{"negative for a different iface is ignored", "eth0", []string{"-wlan0", "eth0"}, true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := Match(tt.ifname, tt.patterns); got != tt.want {
				t.Errorf("Match(%q, %v) = %v, want %v", tt.ifname, tt.patterns, got, tt.want)
			}
		})
	}
}

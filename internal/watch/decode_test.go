package watch

import (
	"net"
	"reflect"
	"testing"

	"golang.org/x/sys/unix"
)

func TestFamily(t *testing.T) {
	cases := []struct {
		ip   string
		want string
	}{
		{"192.0.2.5", "inet"},
		{"127.0.0.1", "inet"},
		{"2001:db8::5", "inet6"},
		{"fe80::1", "inet6"},
	}
	for _, c := range cases {
		if got := family(net.ParseIP(c.ip)); got != c.want {
			t.Errorf("family(%s) = %q, want %q", c.ip, got, c.want)
		}
	}
}

func TestScopeName(t *testing.T) {
	cases := []struct {
		scope int
		want  string
	}{
		{unix.RT_SCOPE_UNIVERSE, "global"},
		{unix.RT_SCOPE_LINK, "link"},
		{unix.RT_SCOPE_HOST, "host"},
		{unix.RT_SCOPE_SITE, "site"},
		{42, "scope(42)"},
	}
	for _, c := range cases {
		if got := scopeName(c.scope); got != c.want {
			t.Errorf("scopeName(%d) = %q, want %q", c.scope, got, c.want)
		}
	}
}

func TestDecodeAddrFlags(t *testing.T) {
	cases := []struct {
		name  string
		flags int
		want  []string
	}{
		{"none", 0, nil},
		{"permanent", unix.IFA_F_PERMANENT, []string{"permanent"}},
		{
			"tentative permanent",
			unix.IFA_F_TENTATIVE | unix.IFA_F_PERMANENT,
			[]string{"tentative", "permanent"},
		},
		{
			"deprecated",
			unix.IFA_F_DEPRECATED | unix.IFA_F_NOPREFIXROUTE,
			[]string{"deprecated", "noprefixroute"},
		},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			if got := decodeAddrFlags(c.flags); !reflect.DeepEqual(got, c.want) {
				t.Errorf("decodeAddrFlags(%#x) = %v, want %v", c.flags, got, c.want)
			}
		})
	}
}

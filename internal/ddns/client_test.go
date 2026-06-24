package ddns

import (
	"context"
	"net/http"
	"net/http/httptest"
	"net/netip"
	"testing"

	"github.com/tmshlvck/teleddns-go/internal/config"
)

func TestSanitizeURL(t *testing.T) {
	tests := []struct {
		in, want string
	}{
		{
			"https://user:secret@host.example.com/ddns/update?myip=2001:db8::1&hostname=h",
			"https://user:<PASSWORD>@host.example.com/ddns/update?myip=2001:db8::1&hostname=h",
		},
		{ // no userinfo: unchanged
			"https://host.example.com/ddns/update?myip=1.2.3.4",
			"https://host.example.com/ddns/update?myip=1.2.3.4",
		},
		{ // username only, no password, no '@'-preceded colon span -> unchanged-ish
			"http://host:8080/path",
			"http://host:8080/path",
		},
	}
	for _, tt := range tests {
		if got := SanitizeURL(tt.in); got != tt.want {
			t.Errorf("SanitizeURL(%q) = %q, want %q", tt.in, got, tt.want)
		}
	}
}

func TestPushSuccessSendsBasicAuthAndParams(t *testing.T) {
	var gotUser, gotPass, gotMyip, gotHost string
	var gotAuthOK bool
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotUser, gotPass, gotAuthOK = r.BasicAuth()
		gotMyip = r.URL.Query().Get("myip")
		gotHost = r.URL.Query().Get("hostname")
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte("good"))
	}))
	defer srv.Close()

	// Inject credentials into the test server URL.
	u := "http://alice:s3cr3t@" + srv.Listener.Addr().String() + "/ddns/update"
	c := New(&config.Config{DDNSURL: u, Hostname: "host.ddns.example.com"})
	if err := c.Push(context.Background(), netip.MustParseAddr("2606:4700::1111")); err != nil {
		t.Fatalf("Push error: %v", err)
	}
	if !gotAuthOK || gotUser != "alice" || gotPass != "s3cr3t" {
		t.Errorf("basic auth = (%q,%q,%v), want (alice,s3cr3t,true)", gotUser, gotPass, gotAuthOK)
	}
	if gotMyip != "2606:4700::1111" {
		t.Errorf("myip = %q", gotMyip)
	}
	if gotHost != "host.ddns.example.com" {
		t.Errorf("hostname = %q", gotHost)
	}
}

func TestPushNon200IsError(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		http.Error(w, "nope", http.StatusInternalServerError)
	}))
	defer srv.Close()

	c := New(&config.Config{DDNSURL: srv.URL + "/ddns/update", Hostname: "h"})
	if err := c.Push(context.Background(), netip.MustParseAddr("1.2.3.4")); err == nil {
		t.Fatalf("expected error on HTTP 500, got nil")
	}
}

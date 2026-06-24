// Package ddns sends dynamic DNS updates to a teleddns-server over HTTP.
//
// The update is an HTTP GET to the configured ddns_url with myip and hostname
// query parameters appended. Credentials embedded in the URL userinfo
// (https://user:pass@host/...) are forwarded as HTTP Basic authentication, and
// the password is redacted whenever the URL is logged.
package ddns

import (
	"context"
	"fmt"
	"io"
	"log/slog"
	"net"
	"net/http"
	"net/netip"
	"net/url"
	"strings"
	"time"

	"github.com/tmshlvck/teleddns-go/internal/config"
)

const (
	totalTimeout = 30 * time.Second
	dialTimeout  = 10 * time.Second
	maxBodyLog   = 512
)

// Client performs DDNS updates against a fixed server URL and hostname.
type Client struct {
	cfg *config.Config
	hc  *http.Client
}

// New returns a Client with sensible HTTP timeouts.
func New(cfg *config.Config) *Client {
	return &Client{
		cfg: cfg,
		hc: &http.Client{
			Timeout: totalTimeout,
			Transport: &http.Transport{
				DialContext:           (&net.Dialer{Timeout: dialTimeout}).DialContext,
				TLSHandshakeTimeout:   dialTimeout,
				ResponseHeaderTimeout: totalTimeout,
				ForceAttemptHTTP2:     true,
			},
		},
	}
}

// Push reports ip for the configured hostname. It returns nil only on an HTTP
// 200 response; on any transport error or non-200 status it returns an error
// so the caller can leave its "last pushed" state pending and retry later.
func (c *Client) Push(ctx context.Context, ip netip.Addr) error {
	u, err := url.Parse(c.cfg.DDNSURL)
	if err != nil {
		return fmt.Errorf("parse ddns_url: %w", err)
	}
	q := u.Query()
	q.Set("myip", ip.String())
	q.Set("hostname", c.cfg.Hostname)
	u.RawQuery = q.Encode()

	// Sanitize for logging while userinfo is still on the URL, then strip it
	// from the request URL and forward it as an explicit Basic auth header.
	sanitized := SanitizeURL(u.String())
	var user, pass string
	var hasAuth bool
	if u.User != nil {
		user = u.User.Username()
		pass, _ = u.User.Password()
		hasAuth = true
		u.User = nil
	}

	req, err := http.NewRequestWithContext(ctx, http.MethodGet, u.String(), nil)
	if err != nil {
		return fmt.Errorf("build request: %w", err)
	}
	if hasAuth {
		req.SetBasicAuth(user, pass)
	}

	slog.Info("sending DDNS update", "ip", ip.String(), "hostname", c.cfg.Hostname, "url", sanitized)
	resp, err := c.hc.Do(req)
	if err != nil {
		slog.Warn("DDNS update failed", "url", sanitized, "err", err)
		return fmt.Errorf("ddns GET: %w", err)
	}
	defer resp.Body.Close()

	body, _ := io.ReadAll(io.LimitReader(resp.Body, maxBodyLog))
	if resp.StatusCode != http.StatusOK {
		slog.Warn("DDNS update rejected", "url", sanitized, "status", resp.StatusCode, "body", strings.TrimSpace(string(body)))
		return fmt.Errorf("ddns GET status %d", resp.StatusCode)
	}
	slog.Info("DDNS update succeeded", "url", sanitized, "status", resp.StatusCode, "body", strings.TrimSpace(string(body)))
	return nil
}

// SanitizeURL redacts the password in a URL string for logging. It mirrors the
// Rust client's sanitize_url: the span between the second ':' (the one before
// the password) and the '@' is replaced with ":<PASSWORD>". If the URL has no
// such userinfo, it is returned unchanged.
func SanitizeURL(u string) string {
	ps := nthIndex(u, ':', 2)
	pe := strings.IndexByte(u, '@')
	if ps < 0 || pe < 0 || ps >= pe {
		return u
	}
	return u[:ps] + ":<PASSWORD>" + u[pe:]
}

// nthIndex returns the byte index of the n-th (1-based) occurrence of b in s,
// or -1 if there are fewer than n occurrences.
func nthIndex(s string, b byte, n int) int {
	count := 0
	for i := 0; i < len(s); i++ {
		if s[i] == b {
			count++
			if count == n {
				return i
			}
		}
	}
	return -1
}

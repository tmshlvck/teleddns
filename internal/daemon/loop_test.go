package daemon

import (
	"context"
	"net/http"
	"net/http/httptest"
	"net/netip"
	"sync/atomic"
	"testing"
	"time"

	"github.com/tmshlvck/teleddns/internal/config"
	"github.com/tmshlvck/teleddns/internal/ddns"
	"github.com/tmshlvck/teleddns/internal/state"
)

// testInterval keeps the worker's rate-limit window short enough that a test
// observes several cycles in well under a second.
const testInterval = 40 * time.Millisecond

// pushRecorder is a DDNS server stand-in that reports every myip it is asked to
// publish and can be switched to failing mid-test.
type pushRecorder struct {
	pushes chan string
	fail   atomic.Bool
}

func newTestWorker(t *testing.T) (*Worker, *pushRecorder) {
	t.Helper()
	rec := &pushRecorder{pushes: make(chan string, 16)}
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if rec.fail.Load() {
			http.Error(w, "nope", http.StatusInternalServerError)
			// Record the attempt even when refusing it, so a test can tell a
			// failed push apart from no push at all.
			rec.pushes <- "fail:" + r.URL.Query().Get("myip")
			return
		}
		rec.pushes <- r.URL.Query().Get("myip")
		w.WriteHeader(http.StatusOK)
	}))
	t.Cleanup(srv.Close)

	cfg := &config.Config{
		DDNSURL:    srv.URL + "/ddns/update",
		Hostname:   "h.example.com",
		EnableIPv6: true,
		Interfaces: []string{"*"},
	}
	return &Worker{cfg: cfg, client: ddns.New(cfg), minInterval: testInterval}, rec
}

// mapWith builds an address map holding a single global IPv6 on a wired iface.
func mapWith(t *testing.T, addr string) state.Map {
	t.Helper()
	cfg := &config.Config{EnableIPv6: true, Interfaces: []string{"*"}}
	m := state.Build([]state.AddrInput{{
		Iface: "eth0", Addr: netip.MustParseAddr(addr), PrefixLen: 64,
	}}, cfg)
	if len(m) != 1 {
		t.Fatalf("built map has %d entries, want 1", len(m))
	}
	return m
}

// waitPush returns the next myip the worker published, failing the test if none
// arrives. The budget covers a coalescing window plus a full cycle.
func waitPush(t *testing.T, rec *pushRecorder) string {
	t.Helper()
	select {
	case ip := <-rec.pushes:
		return ip
	case <-time.After(3 * time.Second):
		t.Fatal("timed out waiting for a DDNS push")
		return ""
	}
}

// expectNoPush asserts the worker stays quiet for several cycles.
func expectNoPush(t *testing.T, rec *pushRecorder) {
	t.Helper()
	select {
	case ip := <-rec.pushes:
		t.Fatalf("unexpected DDNS push of %q", ip)
	case <-time.After(6 * testInterval):
	}
}

// runWorker starts w and returns its update channel; the worker is stopped when
// the test ends.
func runWorker(t *testing.T, w *Worker) chan<- Update {
	t.Helper()
	ctx, cancel := context.WithCancel(context.Background())
	updates := make(chan Update, 8)
	done := make(chan struct{})
	go func() {
		defer close(done)
		w.Run(ctx, updates)
	}()
	t.Cleanup(func() {
		cancel()
		<-done
	})
	return updates
}

// An unchanged map must not be re-pushed, but the same map marked Force must be.
// This is the keepalive: the address never changed, the server hears about it
// again anyway.
func TestForceRepushesUnchangedAddress(t *testing.T) {
	w, rec := newTestWorker(t)
	updates := runWorker(t, w)
	m := mapWith(t, "2606:4700::1111")

	updates <- Update{Map: m}
	if ip := waitPush(t, rec); ip != "2606:4700::1111" {
		t.Fatalf("initial push = %q", ip)
	}

	// Same map, no force: the worker already published this address.
	updates <- Update{Map: m}
	expectNoPush(t, rec)

	updates <- Update{Map: m, Force: true}
	if ip := waitPush(t, rec); ip != "2606:4700::1111" {
		t.Fatalf("forced push = %q", ip)
	}
	// The force is one-shot: it must not leave the worker re-pushing forever.
	expectNoPush(t, rec)
}

// A Force coalesced with a later plain update must still push. The plain map
// supersedes the forced one (it is fresher) but cannot cancel the keepalive.
func TestForceSurvivesCoalescing(t *testing.T) {
	w, rec := newTestWorker(t)
	updates := runWorker(t, w)
	m := mapWith(t, "2606:4700::1111")

	updates <- Update{Map: m}
	waitPush(t, rec)

	// Both land inside one rate-limit window and collapse into one cycle.
	updates <- Update{Map: m, Force: true}
	updates <- Update{Map: m}
	if ip := waitPush(t, rec); ip != "2606:4700::1111" {
		t.Fatalf("coalesced forced push = %q", ip)
	}
	expectNoPush(t, rec)
}

// A forced push that the server rejects must be retried, and must stop once it
// succeeds. Without retention of the force flag the keepalive would be silently
// lost until the next resync tick, a day later.
func TestFailedForceIsRetriedThenSettles(t *testing.T) {
	w, rec := newTestWorker(t)
	updates := runWorker(t, w)
	m := mapWith(t, "2606:4700::1111")

	updates <- Update{Map: m}
	waitPush(t, rec)

	rec.fail.Store(true)
	updates <- Update{Map: m, Force: true}
	for i := 0; i < 2; i++ {
		if ip := waitPush(t, rec); ip != "fail:2606:4700::1111" {
			t.Fatalf("attempt %d = %q, want a failed push", i, ip)
		}
	}

	rec.fail.Store(false)
	// Drain any failures already in flight, then require a success.
	deadline := time.After(3 * time.Second)
	for {
		select {
		case ip := <-rec.pushes:
			if ip == "2606:4700::1111" {
				expectNoPush(t, rec) // force cleared; no further retries
				return
			}
		case <-deadline:
			t.Fatal("forced push never succeeded after the server recovered")
		}
	}
}

// A real address change still pushes without any force, and a family that loses
// its address stops being published.
func TestChangedAddressPushes(t *testing.T) {
	w, rec := newTestWorker(t)
	updates := runWorker(t, w)

	updates <- Update{Map: mapWith(t, "2606:4700::1111")}
	if ip := waitPush(t, rec); ip != "2606:4700::1111" {
		t.Fatalf("first push = %q", ip)
	}
	updates <- Update{Map: mapWith(t, "2606:4700::1112")}
	if ip := waitPush(t, rec); ip != "2606:4700::1112" {
		t.Fatalf("second push = %q", ip)
	}
	expectNoPush(t, rec)
}

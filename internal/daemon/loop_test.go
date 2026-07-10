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
// observes several cycles in well under a second. testMaxBackoff caps the retry
// delay proportionally.
const (
	testInterval   = 40 * time.Millisecond
	testMaxBackoff = 200 * time.Millisecond
)

// pushRecorder is a DDNS server stand-in that reports every myip it is asked to
// publish and can be switched to failing mid-test, per family.
type pushRecorder struct {
	pushes   chan string
	fail     atomic.Bool // reject every push
	failIPv4 atomic.Bool // reject only IPv4 pushes
}

func (rec *pushRecorder) handle(w http.ResponseWriter, r *http.Request) {
	ip := r.URL.Query().Get("myip")
	isV4 := netip.MustParseAddr(ip).Is4()
	if rec.fail.Load() || (isV4 && rec.failIPv4.Load()) {
		// Record the attempt even when refusing it, so a test can tell a failed
		// push apart from no push at all.
		rec.pushes <- "fail:" + ip
		http.Error(w, "nope", http.StatusInternalServerError)
		return
	}
	rec.pushes <- ip
	w.WriteHeader(http.StatusOK)
}

func newTestWorker(t *testing.T) (*Worker, *pushRecorder) {
	t.Helper()
	rec := &pushRecorder{pushes: make(chan string, 32)}
	srv := httptest.NewServer(http.HandlerFunc(rec.handle))
	t.Cleanup(srv.Close)

	cfg := &config.Config{
		DDNSURL:    srv.URL + "/ddns/update",
		Hostname:   "h.example.com",
		EnableIPv6: true,
		EnableIPv4: true,
		Interfaces: []string{"*"},
	}
	w := &Worker{
		cfg:         cfg,
		client:      ddns.New(cfg),
		minInterval: testInterval,
		maxBackoff:  testMaxBackoff,
		// Keep the slack proportional to minInterval, or an idle cycle would be
		// dominated by it and expectNoPush would pass without a cycle running.
		cycleSlack: testInterval / 4,
	}
	return w, rec
}

// mapWith builds an address map from global addresses on a wired interface.
func mapWith(t *testing.T, addrs ...string) state.Map {
	t.Helper()
	cfg := &config.Config{EnableIPv6: true, EnableIPv4: true, Interfaces: []string{"*"}}
	inputs := make([]state.AddrInput, 0, len(addrs))
	for _, a := range addrs {
		addr := netip.MustParseAddr(a)
		plen := 64
		if addr.Is4() {
			plen = 24
		}
		inputs = append(inputs, state.AddrInput{Iface: "eth0", Addr: addr, PrefixLen: plen})
	}
	m := state.Build(inputs, cfg)
	if len(m) != len(addrs) {
		t.Fatalf("built map has %d entries, want %d", len(m), len(addrs))
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

// The families are independent: a forced push that IPv4 keeps rejecting must
// not make IPv6 re-push, and must not stop IPv4 retrying.
func TestFamiliesAreIndependent(t *testing.T) {
	w, rec := newTestWorker(t)
	updates := runWorker(t, w)
	// A routable v4: documentation and RFC1918 ranges score 0 and are never
	// selected, so they would give the worker nothing to push.
	m := mapWith(t, "2606:4700::1111", "9.9.9.9")

	rec.failIPv4.Store(true)
	updates <- Update{Map: m, Force: true}

	var v6Pushes, v4Failures int
	deadline := time.After(3 * time.Second)
	for v4Failures < 3 {
		select {
		case ip := <-rec.pushes:
			switch ip {
			case "2606:4700::1111":
				v6Pushes++
			case "fail:9.9.9.9":
				v4Failures++
			default:
				t.Fatalf("unexpected push %q", ip)
			}
		case <-deadline:
			t.Fatalf("only saw %d IPv4 retries, want 3", v4Failures)
		}
	}
	if v6Pushes != 1 {
		t.Errorf("IPv6 pushed %d times while IPv4 was failing, want exactly 1", v6Pushes)
	}
}

// A successful push must publish its time, since the main loop arms the resync
// deadline from it. A failed push must not.
func TestLastPushOKTracksSuccessOnly(t *testing.T) {
	w, rec := newTestWorker(t)
	if !w.LastPushOK().IsZero() {
		t.Fatalf("LastPushOK() = %v before any push, want zero", w.LastPushOK())
	}
	updates := runWorker(t, w)

	rec.fail.Store(true)
	updates <- Update{Map: mapWith(t, "2606:4700::1111")}
	waitPush(t, rec) // the rejected attempt
	if !w.LastPushOK().IsZero() {
		t.Errorf("LastPushOK() = %v after a failed push, want zero", w.LastPushOK())
	}

	rec.fail.Store(false)
	before := time.Now()
	for {
		if ip := waitPush(t, rec); ip == "2606:4700::1111" {
			break
		}
	}
	// The stub records the request before responding, so the worker has not
	// necessarily stored the time yet when waitPush returns. Poll for it.
	var got time.Time
	for deadline := time.Now().Add(2 * time.Second); time.Now().Before(deadline); {
		if got = w.LastPushOK(); !got.IsZero() {
			break
		}
		time.Sleep(5 * time.Millisecond)
	}
	// The worker timestamps the cycle, which starts before the round trip.
	if got.IsZero() || got.Before(before.Add(-time.Second)) {
		t.Errorf("LastPushOK() = %v, want a time at or after %v", got, before)
	}
}

func TestBackoffDoublesAndCaps(t *testing.T) {
	w := &Worker{minInterval: 30 * time.Second, maxBackoff: 15 * time.Minute}
	for _, tc := range []struct {
		failures int
		want     time.Duration
	}{
		{1, 30 * time.Second},
		{2, time.Minute},
		{3, 2 * time.Minute},
		{4, 4 * time.Minute},
		{5, 8 * time.Minute},
		{6, 15 * time.Minute}, // 16m capped
		{7, 15 * time.Minute},
		{64, 15 * time.Minute}, // would overflow the shift
	} {
		if got := w.backoff(tc.failures); got != tc.want {
			t.Errorf("backoff(%d) = %s, want %s", tc.failures, got, tc.want)
		}
	}
}

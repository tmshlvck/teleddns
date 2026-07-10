// Package daemon contains the rate-limited update worker that turns a stream of
// address maps into DDNS pushes and hook runs.
package daemon

import (
	"context"
	"log/slog"
	"net/netip"
	"sync/atomic"
	"time"

	"github.com/tmshlvck/teleddns/internal/config"
	"github.com/tmshlvck/teleddns/internal/ddns"
	"github.com/tmshlvck/teleddns/internal/hooks"
	"github.com/tmshlvck/teleddns/internal/state"
)

// MinUpdateInterval is the minimum time between update cycles. Changes arriving
// inside the window are coalesced and applied when it expires. It matches the
// Rust client's MIN_UPDATE_INTERVAL_S. It is also the first retry delay after a
// failed push, from which the backoff doubles.
const MinUpdateInterval = 30 * time.Second

// MaxBackoff caps the exponential retry delay after repeated push failures.
const MaxBackoff = 15 * time.Minute

// maxShift bounds the doubling so the shift in backoff cannot overflow.
const maxShift = 20

// Update is one observation of the address state, handed to the worker.
//
// Force asks for a DDNS push of every family even when the selected addresses
// are unchanged — the periodic keepalive that repairs a record the server lost.
// The caller sets it only on a Map it has just rebuilt from a full netlink
// dump. Carrying the request on the map rather than on a clock inside the
// worker is what guarantees a keepalive push reports freshly dumped state:
// there is no way to force a push except by handing the worker a map to push.
//
// The worker then tracks the force per family, so one family's failure neither
// re-pushes nor delays the other.
type Update struct {
	Map   state.Map
	Force bool
}

// Worker consumes address maps, runs hooks on every change, and pushes the best
// per-family address when it differs from the last successful push or when an
// Update asks for a forced re-push.
type Worker struct {
	cfg    *config.Config
	client *ddns.Client
	// minInterval is MinUpdateInterval, lowered by tests to keep them quick. It
	// doubles as the first retry delay, and maxBackoff (MaxBackoff) caps it.
	minInterval time.Duration
	maxBackoff  time.Duration
	// cycleSlack is how far past the rate-limit window an idle wake-up lands,
	// so the window is certainly closed when the cycle runs. Tests shrink it
	// along with minInterval so a test cycle is not dominated by the slack.
	cycleSlack time.Duration
	// lastPushOK is the Unix-nanosecond time of the last successful push of any
	// family, or 0 if none has succeeded. It is read by the main loop, which
	// arms the resync deadline from it, so a keepalive is skipped whenever an
	// ordinary push has already reported the address recently.
	lastPushOK atomic.Int64
}

// NewWorker builds a Worker with a DDNS client for cfg.
func NewWorker(cfg *config.Config) *Worker {
	return &Worker{
		cfg:         cfg,
		client:      ddns.New(cfg),
		minInterval: MinUpdateInterval,
		maxBackoff:  MaxBackoff,
		cycleSlack:  time.Second,
	}
}

// LastPushOK reports when a push last succeeded, or the zero time if none has.
// It is safe to call from another goroutine.
func (w *Worker) LastPushOK() time.Time {
	ns := w.lastPushOK.Load()
	if ns == 0 {
		return time.Time{}
	}
	return time.Unix(0, ns)
}

// famState is the per-family push state. Everything here is per address family
// so a failing IPv4 server never delays, retries, or re-pushes IPv6.
type famState struct {
	// pending is the address we want published; current is the last one we
	// published successfully.
	pending, current netip.Addr
	// force re-pushes pending even when it equals current (the keepalive). It
	// is retained across a failed push so the keepalive is retried rather than
	// skipped until the next resync deadline.
	force bool
	// failures counts consecutive failed pushes; nextAttempt is when the
	// backoff permits the next one. Both reset on success.
	failures    int
	nextAttempt time.Time
}

// backoff returns the delay after n consecutive failures: minInterval doubled
// per failure, capped at maxBackoff. There is no jitter — a host runs one
// client, so there is no herd to spread out.
func (w *Worker) backoff(failures int) time.Duration {
	if failures > maxShift {
		return w.maxBackoff
	}
	d := w.minInterval << (failures - 1)
	if d <= 0 || d > w.maxBackoff {
		return w.maxBackoff
	}
	return d
}

// Run consumes updates until the channel is closed or ctx is done.
//
// This is a port of the Rust update_worker: incoming maps are dampened to at
// most one cycle per minInterval; a cycle applies hooks and recomputes the best
// addresses only when the map actually changed, and pushes a family only when
// its best differs from the last successfully pushed value.
//
// A forced Update additionally re-pushes unchanged addresses. The flag survives
// coalescing: a plain change arriving during the rate-limit window supersedes
// the forced map (it is strictly fresher) but does not cancel the keepalive.
//
// A failed push does not update the family's last-pushed address, so it is
// retried, subject to that family's exponential backoff. Because the retry
// reuses the map already held, the address it reports is at most one
// redump_interval stale.
func (w *Worker) Run(ctx context.Context, updates <-chan Update) {
	iterationStart := time.Now()
	var (
		currentMap  state.Map
		haveCurrent bool
		newMap      state.Map
		haveNew     bool

		v6, v4 famState
	)
	nextTimeout := w.minInterval + w.cycleSlack

	for {
		timer := time.NewTimer(nextTimeout)
		select {
		case <-ctx.Done():
			timer.Stop()
			slog.Info("update worker stopping", "reason", "context canceled")
			return
		case u, ok := <-updates:
			timer.Stop()
			if !ok {
				slog.Info("update worker stopping", "reason", "update channel closed")
				return
			}
			newMap, haveNew = u.Map, true
			if u.Force {
				// Sticky per family until each is pushed or has nothing to push.
				v6.force, v4.force = true, true
			}
			if elapsed := time.Since(iterationStart); elapsed < w.minInterval {
				nextTimeout = w.minInterval - elapsed + w.cycleSlack
				slog.Debug("rate-limit active, coalescing changes until window expires",
					"elapsed", elapsed.Round(time.Second), "next_in", nextTimeout.Round(time.Second))
				continue
			}
		case <-timer.C:
			// Window elapsed: process whatever state we last saw.
		}

		now := time.Now()
		changed := haveNew && (!haveCurrent || !newMap.Equal(currentMap))
		switch {
		case changed:
			slog.Info("address state changed, running update", "addresses", newMap.Summary())
			hooks.Apply(ctx, newMap, w.cfg)
			v6.pending, v4.pending = newMap.SelectBest(w.cfg)
			slog.Info("selected best addresses", "best6", addrOrNone(v6.pending), "best4", addrOrNone(v4.pending))
			currentMap, haveCurrent = newMap, true
		case v6.force || v4.force:
			slog.Info("periodic resync, re-pushing current addresses",
				"best6", addrOrNone(v6.pending), "best4", addrOrNone(v4.pending))
		}

		w.sync(ctx, "ipv6", &v6, now)
		w.sync(ctx, "ipv4", &v4, now)

		iterationStart = time.Now()
		nextTimeout = w.minInterval + w.cycleSlack
	}
}

// sync pushes f.pending when it differs from the last successful push, or when
// f.force asks for a re-push of an unchanged address, unless the family is
// still inside its backoff window. A successful push clears the force and the
// backoff; a failed one keeps the force and lengthens the backoff.
func (w *Worker) sync(ctx context.Context, family string, f *famState, now time.Time) {
	if !f.pending.IsValid() {
		// Nothing to report for this family. Forget what we last pushed, so a
		// later reappearance of the same address pushes again, and drop any
		// force and backoff along with it.
		*f = famState{}
		return
	}
	if f.pending == f.current && !f.force {
		return
	}
	if now.Before(f.nextAttempt) {
		slog.Debug("DDNS push deferred, backing off after failures",
			"family", family, "ip", f.pending, "failures", f.failures,
			"retry_in", f.nextAttempt.Sub(now).Round(time.Second))
		return
	}

	if err := w.client.Push(ctx, f.pending); err != nil {
		f.failures++
		delay := w.backoff(f.failures)
		f.nextAttempt = now.Add(delay)
		slog.Warn("DDNS push failed, backing off",
			"family", family, "ip", f.pending, "failures", f.failures,
			"retry_in", delay.Round(time.Second))
		return
	}
	f.current, f.force = f.pending, false
	f.failures, f.nextAttempt = 0, time.Time{}
	w.lastPushOK.Store(now.UnixNano())
}

func addrOrNone(a netip.Addr) string {
	if !a.IsValid() {
		return "none"
	}
	return a.String()
}

// Package daemon contains the rate-limited update worker that turns a stream of
// address maps into DDNS pushes and hook runs.
package daemon

import (
	"context"
	"log/slog"
	"net/netip"
	"time"

	"github.com/tmshlvck/teleddns/internal/config"
	"github.com/tmshlvck/teleddns/internal/ddns"
	"github.com/tmshlvck/teleddns/internal/hooks"
	"github.com/tmshlvck/teleddns/internal/state"
)

// MinUpdateInterval is the minimum time between update cycles. Changes arriving
// inside the window are coalesced and applied when it expires. It matches the
// Rust client's MIN_UPDATE_INTERVAL_S.
const MinUpdateInterval = 30 * time.Second

// Update is one observation of the address state, handed to the worker.
//
// Force asks for a DDNS push even when the selected addresses are unchanged —
// the periodic keepalive that repairs a record the server lost. The caller sets
// it only on a Map it has just rebuilt from a full netlink dump. Carrying the
// request on the map rather than on a clock inside the worker is what
// guarantees a keepalive push reports freshly dumped state: there is no way to
// force a push except by handing the worker a map to push.
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
	// minInterval is MinUpdateInterval, lowered by tests to keep them quick.
	minInterval time.Duration
}

// NewWorker builds a Worker with a DDNS client for cfg.
func NewWorker(cfg *config.Config) *Worker {
	return &Worker{cfg: cfg, client: ddns.New(cfg), minInterval: MinUpdateInterval}
}

// famState is the per-family push state: the address we want published, and the
// last one we published successfully.
type famState struct {
	pending, current netip.Addr
}

// Run consumes updates until the channel is closed or ctx is done.
//
// This is a port of the Rust update_worker: incoming maps are dampened to at
// most one cycle per minInterval; a cycle applies hooks and recomputes the best
// addresses only when the map actually changed, and pushes a family only when
// its best differs from the last successfully pushed value. A failed push leaves
// the "last pushed" value untouched so the next cycle retries.
//
// A forced Update additionally re-pushes unchanged addresses. The flag survives
// coalescing: a plain change arriving during the rate-limit window supersedes
// the forced map (it is strictly fresher) but does not cancel the forced push.
// A force whose push fails is retained so the next cycle retries it, exactly as
// a changed-address push is retried.
func (w *Worker) Run(ctx context.Context, updates <-chan Update) {
	iterationStart := time.Now()
	var (
		currentMap  state.Map
		haveCurrent bool
		newMap      state.Map
		haveNew     bool
		force       bool

		v6, v4 famState
	)
	nextTimeout := w.minInterval + time.Second

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
			force = force || u.Force
			if elapsed := time.Since(iterationStart); elapsed < w.minInterval {
				nextTimeout = w.minInterval - elapsed + time.Second
				slog.Debug("rate-limit active, coalescing changes until window expires",
					"elapsed", elapsed.Round(time.Second), "next_in", nextTimeout.Round(time.Second))
				continue
			}
		case <-timer.C:
			// Window elapsed: process whatever state we last saw.
		}

		changed := haveNew && (!haveCurrent || !newMap.Equal(currentMap))
		switch {
		case changed:
			slog.Info("address state changed, running update", "addresses", newMap.Summary())
			hooks.Apply(ctx, newMap, w.cfg)
			v6.pending, v4.pending = newMap.SelectBest(w.cfg)
			slog.Info("selected best addresses", "best6", addrOrNone(v6.pending), "best4", addrOrNone(v4.pending))
			currentMap, haveCurrent = newMap, true
		case force:
			slog.Info("periodic resync, re-pushing current addresses",
				"best6", addrOrNone(v6.pending), "best4", addrOrNone(v4.pending))
		case v6.pending != v6.current || v4.pending != v4.current:
			slog.Info("retrying pending DDNS update", "best6", addrOrNone(v6.pending), "best4", addrOrNone(v4.pending))
		}

		// Clear the force only once every family it applied to is settled; a
		// failed push leaves it set so the next cycle retries the keepalive.
		settled6 := w.sync(ctx, &v6, force)
		settled4 := w.sync(ctx, &v4, force)
		if settled6 && settled4 {
			force = false
		}

		iterationStart = time.Now()
		nextTimeout = w.minInterval + time.Second
	}
}

// sync pushes f.pending when it differs from the last successful push, or when
// force asks for a re-push of an unchanged address. It reports whether the
// family is settled: true when the push succeeded or there was nothing to push,
// false when a push was attempted and failed.
func (w *Worker) sync(ctx context.Context, f *famState, force bool) bool {
	if !f.pending.IsValid() {
		// Nothing to report for this family. Forget what we last pushed so a
		// later reappearance of the same address pushes again.
		f.current = netip.Addr{}
		return true
	}
	if f.pending == f.current && !force {
		return true
	}
	if err := w.client.Push(ctx, f.pending); err != nil {
		return false
	}
	f.current = f.pending
	return true
}

func addrOrNone(a netip.Addr) string {
	if !a.IsValid() {
		return "none"
	}
	return a.String()
}

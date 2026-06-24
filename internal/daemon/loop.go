// Package daemon contains the rate-limited update worker that turns a stream of
// address maps into DDNS pushes and hook runs.
package daemon

import (
	"context"
	"log/slog"
	"net/netip"
	"time"

	"github.com/tmshlvck/teleddns-go/internal/config"
	"github.com/tmshlvck/teleddns-go/internal/ddns"
	"github.com/tmshlvck/teleddns-go/internal/hooks"
	"github.com/tmshlvck/teleddns-go/internal/state"
)

// MinUpdateInterval is the minimum time between update cycles. Changes arriving
// inside the window are coalesced and applied when it expires. It matches the
// Rust client's MIN_UPDATE_INTERVAL_S.
const MinUpdateInterval = 30 * time.Second

// Worker consumes address maps, runs hooks on every change, and pushes the
// best per-family address when it differs from the last successful push.
type Worker struct {
	cfg    *config.Config
	client *ddns.Client
}

// NewWorker builds a Worker with a DDNS client for cfg.
func NewWorker(cfg *config.Config) *Worker {
	return &Worker{cfg: cfg, client: ddns.New(cfg)}
}

// Run consumes maps from updates until the channel is closed or ctx is done.
//
// This is a port of the Rust update_worker: incoming maps are dampened to at
// most one cycle per MinUpdateInterval; a cycle applies hooks and recomputes
// the best addresses only when the map actually changed, and pushes a family
// only when its best differs from the last successfully pushed value. A failed
// push leaves the "last pushed" value untouched so the next cycle retries.
func (w *Worker) Run(ctx context.Context, updates <-chan state.Map) {
	iterationStart := time.Now()
	var (
		currentMap  state.Map
		haveCurrent bool
		newMap      state.Map
		haveNew     bool

		currentBest6, currentBest4 netip.Addr
		pendingBest6, pendingBest4 netip.Addr
	)
	nextTimeout := MinUpdateInterval + time.Second

	for {
		timer := time.NewTimer(nextTimeout)
		select {
		case <-ctx.Done():
			timer.Stop()
			slog.Info("update worker stopping", "reason", "context canceled")
			return
		case m, ok := <-updates:
			timer.Stop()
			if !ok {
				slog.Info("update worker stopping", "reason", "update channel closed")
				return
			}
			newMap, haveNew = m, true
			if elapsed := time.Since(iterationStart); elapsed < MinUpdateInterval {
				nextTimeout = MinUpdateInterval - elapsed + time.Second
				slog.Debug("dampening change", "elapsed", elapsed.Round(time.Second), "next_in", nextTimeout.Round(time.Second))
				continue
			}
		case <-timer.C:
			// Window elapsed: process whatever state we last saw.
		}

		changed := haveNew && (!haveCurrent || !newMap.Equal(currentMap))
		if changed {
			slog.Info("address state changed, running update", "addresses", newMap.Summary())
			hooks.Apply(ctx, newMap, w.cfg)
			pendingBest6, pendingBest4 = newMap.SelectBest(w.cfg)
			slog.Info("selected best addresses", "best6", addrOrNone(pendingBest6), "best4", addrOrNone(pendingBest4))
			currentMap, haveCurrent = newMap, true
		} else if pendingBest6 != currentBest6 || pendingBest4 != currentBest4 {
			slog.Info("retrying pending DDNS update", "best6", addrOrNone(pendingBest6), "best4", addrOrNone(pendingBest4))
		}

		if pendingBest6 != currentBest6 {
			if pendingBest6.IsValid() {
				if err := w.client.Push(ctx, pendingBest6); err == nil {
					currentBest6 = pendingBest6
				}
			} else {
				currentBest6 = netip.Addr{}
			}
		}
		if pendingBest4 != currentBest4 {
			if pendingBest4.IsValid() {
				if err := w.client.Push(ctx, pendingBest4); err == nil {
					currentBest4 = pendingBest4
				}
			} else {
				currentBest4 = netip.Addr{}
			}
		}

		iterationStart = time.Now()
		nextTimeout = MinUpdateInterval + time.Second
	}
}

func addrOrNone(a netip.Addr) string {
	if !a.IsValid() {
		return "none"
	}
	return a.String()
}

// Command teleddns is a dynamic DNS client for teleddns-server.
//
// Milestone 2: the netlink observer is now a functional DDNS client. It filters
// interfaces, scores each address by metric, selects the best per family, and
// pushes it to the teleddns-server, rate-limited to one cycle per 30s. Hooks
// (nftables sets, shell) run on every observed state change. See PRD.md.
//
// Copyright 2023-2026 Tomas Hlavacek <tmshlvck@gmail.com>
// SPDX-License-Identifier: GPL-3.0-or-later
package main

import (
	"context"
	"flag"
	"fmt"
	"log/slog"
	"net/netip"
	"os"
	"os/signal"
	"strconv"
	"strings"
	"syscall"
	"time"

	"github.com/tmshlvck/teleddns/internal/applog"
	"github.com/tmshlvck/teleddns/internal/config"
	"github.com/tmshlvck/teleddns/internal/daemon"
	"github.com/tmshlvck/teleddns/internal/ddns"
	"github.com/tmshlvck/teleddns/internal/hooks"
	"github.com/tmshlvck/teleddns/internal/observe"
	"github.com/tmshlvck/teleddns/internal/state"
	"github.com/tmshlvck/teleddns/internal/watch"
)

// version is overridable at build time with -ldflags "-X main.version=...", as
// the RPM and deb packaging does. It must track the released version, because a
// plain `go build` — what the README and deploy.sh tell users to run when no
// package exists — reports this default verbatim.
var version = "0.3.1"

const defaultConfigPath = "/etc/teleddns/teleddns.yaml"

// updateBuffer bounds how many rebuilt maps can queue while the worker is busy
// pushing (network I/O can take up to the DDNS timeout). The worker coalesces,
// so a small buffer is plenty.
const updateBuffer = 16

// dumpRetryDelay is how soon a resync is retried when the netlink dump it needs
// fails. Without a dump there is no fresh map, and a keepalive must never push
// a stale one.
const dumpRetryDelay = time.Minute

// countFlag is a repeatable boolean flag: each occurrence increments it,
// giving the `-v -v` / `-q` verbosity behavior of the Rust client.
type countFlag int

func (c *countFlag) String() string   { return strconv.Itoa(int(*c)) }
func (c *countFlag) Set(string) error { *c++; return nil }
func (c *countFlag) IsBoolFlag() bool { return true }

func main() {
	os.Exit(run())
}

func run() int {
	var (
		cfgPath string
		oneshot bool
		showVer bool
		verbose countFlag
		quiet   countFlag
	)
	fs := flag.NewFlagSet("teleddns", flag.ContinueOnError)
	fs.StringVar(&cfgPath, "config", defaultConfigPath, "config file path")
	fs.StringVar(&cfgPath, "c", defaultConfigPath, "config file path (shorthand)")
	fs.BoolVar(&oneshot, "oneshot", false, "run one update cycle then exit")
	fs.BoolVar(&oneshot, "o", false, "run one update cycle then exit (shorthand)")
	fs.BoolVar(&showVer, "version", false, "print version and exit")
	fs.BoolVar(&showVer, "V", false, "print version and exit (shorthand)")
	fs.Var(&verbose, "verbose", "increase log verbosity (repeatable)")
	fs.Var(&verbose, "v", "increase log verbosity (shorthand, repeatable)")
	fs.Var(&quiet, "quiet", "decrease log verbosity (repeatable)")
	fs.Var(&quiet, "q", "decrease log verbosity (shorthand, repeatable)")

	if err := fs.Parse(os.Args[1:]); err != nil {
		if err == flag.ErrHelp {
			return 0
		}
		return 2
	}

	if showVer {
		fmt.Printf("teleddns %s\n", version)
		return 0
	}

	cfg, err := config.Load(cfgPath)
	if err != nil {
		fmt.Fprintf(os.Stderr, "teleddns: %v\n", err)
		return 1
	}

	level := applog.LevelFor(int(verbose)-int(quiet), cfg.DebugEnabled())
	logger := applog.Setup(level)

	mode := "daemon"
	if oneshot {
		mode = "oneshot"
	}
	logger.Info("teleddns starting",
		"version", version, "config", cfgPath, "mode", mode,
		"interfaces", cfg.Interfaces, "enable_ipv6", cfg.EnableIPv6, "enable_ipv4", cfg.EnableIPv4)

	// SIGINT, SIGTERM and SIGHUP all initiate graceful shutdown. The Rust
	// client only honored SIGHUP; the Go port handles all three.
	ctx, stop := signal.NotifyContext(context.Background(),
		syscall.SIGINT, syscall.SIGTERM, syscall.SIGHUP)
	defer stop()

	if oneshot {
		return runOneshot(ctx, cfg, logger)
	}
	return runDaemon(ctx, cfg, logger)
}

// runOneshot builds the current state once, runs hooks, and pushes the best
// per-family address, then exits. It returns non-zero if a push failed so the
// mode is usable from cron.
func runOneshot(ctx context.Context, cfg *config.Config, logger *slog.Logger) int {
	m, err := state.Dump(cfg)
	if err != nil {
		logger.Error("state build failed", "err", err)
		return 1
	}
	logger.Info("address state", "addresses", m.Summary())
	hooks.Apply(ctx, m, cfg)

	best6, best4 := m.SelectBest(cfg)
	logger.Info("selected best addresses", "best6", addrOrNone(best6), "best4", addrOrNone(best4))

	client := ddns.New(cfg)
	rc := 0
	if best6.IsValid() {
		if err := client.Push(ctx, best6); err != nil {
			rc = 1
		}
	}
	if best4.IsValid() {
		if err := client.Push(ctx, best4); err != nil {
			rc = 1
		}
	}
	return rc
}

// runDaemon watches netlink for changes, rebuilds the address map on every
// change, and feeds the rate-limited update worker.
func runDaemon(ctx context.Context, cfg *config.Config, logger *slog.Logger) int {
	w := watch.New()
	dump, err := w.Dump()
	if err != nil {
		logger.Error("initial state dump failed", "err", err)
		return 1
	}
	links, addrs := 0, 0
	for _, e := range dump {
		switch e.Kind {
		case watch.EventNewLink:
			links++
		case watch.EventNewAddr:
			addrs++
		}
		observe.Print(logger, e)
	}
	logger.Info("initial state dump complete", "links", links, "addresses", addrs)

	// Start the update worker and prime it with the initial state so a first
	// push happens without waiting for an external change.
	updates := make(chan daemon.Update, updateBuffer)
	worker := daemon.NewWorker(cfg)
	workerDone := make(chan struct{})
	go func() {
		defer close(workerDone)
		worker.Run(ctx, updates)
	}()

	// known is the last address map we built. It is the table we consult to
	// decide whether an incoming NEWADDR is a no-op re-announcement (already
	// known, unchanged flags) and can be ignored without a rebuild — mirroring
	// the Rust client's iface_addrs_map dampening.
	known, err := state.Dump(cfg)
	if err != nil {
		logger.Error("initial state build failed", "err", err)
		known = state.Map{}
	} else {
		// The resync deadline starts expired: the first push is the startup
		// resync, performed by the worker's ordinary cycle. Nothing else here
		// treats startup as a special case.
		updates <- daemon.Update{Map: known, Force: true}
	}

	evDone := make(chan struct{})
	events, err := w.Subscribe(evDone)
	if err != nil {
		logger.Error("netlink subscription failed", "err", err,
			"hint", "multicast subscription needs CAP_NET_ADMIN or root")
		close(evDone)
		close(updates)
		<-workerDone
		return 1
	}
	logger.Info("watching netlink events; send SIGINT/SIGTERM/SIGHUP to stop",
		"redump_interval", intervalOrOff(cfg.Redump()), "resync_interval", intervalOrOff(cfg.Resync()))

	running := true

	// reconcile rebuilds the address map from a full netlink dump and hands it
	// to the worker. A dump that disagrees with the state the event stream left
	// us in means the kernel dropped a multicast message — the reason the
	// periodic re-dump exists — so say so.
	//
	// force marks the update as the periodic resync: the worker then re-pushes
	// even an unchanged address. Because the only way to force a push is to
	// hand the worker a map, and this map was just dumped, a resync push always
	// follows a fresh dump and never precedes one.
	reconcile := func(force bool) (sent bool) {
		m, err := state.Dump(cfg)
		if err != nil {
			logger.Error("periodic re-dump failed", "err", err, "resync_pending", force)
			return false
		}
		if !m.Equal(known) {
			logger.Warn("re-dump found state the netlink event stream missed",
				"addresses", m.Summary())
		}
		known = m
		select {
		case updates <- daemon.Update{Map: m, Force: force}:
			return true
		case <-ctx.Done():
			running = false
			return false
		}
	}

	redumpC, stopRedump := ticker(cfg.Redump())
	defer stopRedump()

	// The resync is a deadline, not a tick: it measures time since the last
	// successful push, whatever caused it, so an address change resets it and a
	// busy host never sends a redundant keepalive. When the timer fires we ask
	// the worker when it last succeeded; if that was recent the keepalive is
	// not due and we simply re-arm for the remainder.
	resync := cfg.Resync()
	var resyncC <-chan time.Time
	var resyncTimer *time.Timer
	if resync > 0 {
		resyncTimer = time.NewTimer(resync)
		resyncC = resyncTimer.C
		defer resyncTimer.Stop()
	}

	for running {
		select {
		case <-ctx.Done():
			running = false
		case <-resyncC:
			if remaining := resync - time.Since(worker.LastPushOK()); remaining > 0 {
				logger.Debug("resync not due, a push already reported this address",
					"next_in", remaining.Round(time.Second))
				resyncTimer.Reset(remaining)
				break
			}
			if reconcile(true) {
				resyncTimer.Reset(resync)
			} else {
				// The dump failed, so there is no fresh map to force a push
				// with. Retry soon rather than skip a whole resync period.
				resyncTimer.Reset(dumpRetryDelay)
			}
		case <-redumpC:
			reconcile(false)
		case e, ok := <-events:
			if !ok {
				running = false
				break
			}
			if ignoreEvent(logger, known, e) {
				continue
			}
			m, err := state.Dump(cfg)
			if err != nil {
				logger.Error("state build failed", "err", err)
				continue
			}
			known = m
			select {
			case updates <- daemon.Update{Map: m}:
			case <-ctx.Done():
				running = false
			}
		}
	}

	logger.Info("signal received, shutting down")
	close(evDone)
	close(updates)
	<-workerDone
	logger.Info("shutdown complete")
	return 0
}

func addrOrNone(a netip.Addr) string {
	if !a.IsValid() {
		return "none"
	}
	return a.String()
}

// ticker returns a channel firing every d and a stop function. For d == 0 (the
// periodic task is disabled) it returns a nil channel, which blocks forever in
// a select, so callers need no special case.
func ticker(d time.Duration) (<-chan time.Time, func()) {
	if d <= 0 {
		return nil, func() {}
	}
	t := time.NewTicker(d)
	return t.C, t.Stop
}

func intervalOrOff(d time.Duration) string {
	if d <= 0 {
		return "disabled"
	}
	return d.String()
}

// ignoreEvent reports whether a netlink event can be skipped without rebuilding
// the address map. Mirroring the Rust client, a NEWADDR re-announcement for an
// address we already track with identical flags (typically a SLAAC address
// whose lifetime was refreshed by a Router Advertisement) cannot change the
// selection, so it is logged and ignored. Every other event — a new address, a
// flag change such as DEPRECATED being set, an address removal, or any link
// change — triggers a full rebuild.
func ignoreEvent(logger *slog.Logger, known state.Map, e watch.Event) bool {
	if e.Kind == watch.EventNewAddr && e.Addr != nil {
		if addr, ok := netip.AddrFromSlice(e.Addr.IP); ok {
			addr = addr.Unmap()
			if known.Knows(addr, e.Addr.PrefixLen, e.Addr.FlagBits) {
				logger.Debug("update dampened: address already known and unchanged",
					"addr", fmt.Sprintf("%s/%d", addr, e.Addr.PrefixLen),
					"iface", addrIface(e.Addr),
					"flags", orDash(strings.Join(e.Addr.Flags, ",")))
				return true
			}
		}
	}
	logEventTrigger(logger, e)
	return false
}

// logEventTrigger logs the change that is about to trigger a state rebuild,
// including the interface and address for address events.
func logEventTrigger(logger *slog.Logger, e watch.Event) {
	switch {
	case e.Addr != nil:
		logger.Debug("netlink change triggers update",
			"kind", string(e.Kind),
			"addr", fmt.Sprintf("%s/%d", e.Addr.IP, e.Addr.PrefixLen),
			"iface", addrIface(e.Addr),
			"flags", orDash(strings.Join(e.Addr.Flags, ",")))
	case e.Link != nil:
		logger.Debug("netlink change triggers update",
			"kind", string(e.Kind), "iface", e.Link.Name)
	default:
		logger.Debug("netlink change triggers update", "kind", string(e.Kind))
	}
}

func addrIface(a *watch.AddrState) string {
	if a.Name != "" {
		return a.Name
	}
	return fmt.Sprintf("if%d", a.Index)
}

func orDash(s string) string {
	if s == "" {
		return "-"
	}
	return s
}

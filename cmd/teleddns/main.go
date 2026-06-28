// Command teleddns is the teleddns-go dynamic DNS client.
//
// Milestone 2: the netlink observer is now a functional DDNS client. It filters
// interfaces, scores each address by metric, selects the best per family, and
// pushes it to the teleddns-server, rate-limited to one cycle per 30s. Hooks
// (nftables sets, shell) run on every observed state change. See PRD.md.
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

	"github.com/tmshlvck/teleddns-go/internal/applog"
	"github.com/tmshlvck/teleddns-go/internal/config"
	"github.com/tmshlvck/teleddns-go/internal/daemon"
	"github.com/tmshlvck/teleddns-go/internal/ddns"
	"github.com/tmshlvck/teleddns-go/internal/hooks"
	"github.com/tmshlvck/teleddns-go/internal/observe"
	"github.com/tmshlvck/teleddns-go/internal/state"
	"github.com/tmshlvck/teleddns-go/internal/watch"
)

// version is overridable at build time with -ldflags "-X main.version=...".
// Packaging (RPM/deb) stamps the package version here; this default is only
// seen in `go build` / `go run` development builds.
var version = "0.3.0-dev"

const defaultConfigPath = "/etc/teleddns/teleddns.yaml"

// updateBuffer bounds how many rebuilt maps can queue while the worker is busy
// pushing (network I/O can take up to the DDNS timeout). The worker coalesces,
// so a small buffer is plenty.
const updateBuffer = 16

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
		fmt.Printf("teleddns-go %s\n", version)
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
	logger.Info("teleddns-go starting",
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
	updates := make(chan state.Map, updateBuffer)
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
		updates <- known
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
	logger.Info("watching netlink events; send SIGINT/SIGTERM/SIGHUP to stop")

	for running := true; running; {
		select {
		case <-ctx.Done():
			running = false
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
			case updates <- m:
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

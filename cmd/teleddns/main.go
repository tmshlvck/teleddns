// Command teleddns is the teleddns-go dynamic DNS client.
//
// Milestone 1: open an rtnetlink observer, dump current links and addresses,
// then log every subsequent link/address change. No filtering, selection,
// DDNS push, or hooks yet — see PRD.md.
package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"os/signal"
	"strconv"
	"syscall"

	"github.com/tmshlvck/teleddns-go/internal/applog"
	"github.com/tmshlvck/teleddns-go/internal/config"
	"github.com/tmshlvck/teleddns-go/internal/observe"
	"github.com/tmshlvck/teleddns-go/internal/watch"
)

// version is overridable at build time with -ldflags "-X main.version=...".
var version = "0.1.0-m1"

const defaultConfigPath = "/etc/teleddns/teleddns.yaml"

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
		"interfaces", cfg.Interfaces)

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

	if oneshot {
		logger.Info("oneshot mode: state dumped, exiting")
		return 0
	}

	// SIGINT, SIGTERM and SIGHUP all initiate graceful shutdown. The Rust
	// client only honored SIGHUP; the Go port handles all three.
	ctx, stop := signal.NotifyContext(context.Background(),
		syscall.SIGINT, syscall.SIGTERM, syscall.SIGHUP)
	defer stop()

	done := make(chan struct{})
	events, err := w.Subscribe(done)
	if err != nil {
		logger.Error("netlink subscription failed", "err", err,
			"hint", "multicast subscription needs CAP_NET_ADMIN or root")
		close(done)
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
			observe.Print(logger, e)
		}
	}

	logger.Info("signal received, shutting down")
	close(done)
	logger.Info("shutdown complete")
	return 0
}

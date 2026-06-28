# teleddns-go

A Linux dynamic DNS client. It watches the kernel's `rtnetlink` socket for
network interface and address changes, selects the most appropriate public
address per host, and reports it to a
[teleddns-server](https://github.com/tmshlvck/teleddns-server) over HTTP.

`teleddns-go` is a Go re-implementation of the original Rust client (project
[`teleddns`](https://github.com/tmshlvck/teleddns)). The two are intended to be
deployment-compatible: the Go client reads the same `teleddns.yaml` and accepts
the same CLI flags. See [`PRD.md`](PRD.md) for the full specification and the
milestone plan.

## Status

**Milestones 1 & 2 done.** The client opens an rtnetlink observer, filters
interfaces, scores each address by metric, selects the best per family, and
pushes it to the teleddns-server over HTTP — rate-limited to one cycle per 30s.
Hooks (nftables sets, shell) run on every observed state change.

Milestone 3 (incremental state, dedup, persisted state, backoff) is not started.
Milestone 4 (RPM/deb packaging and CI for the Go build) is in place: see
[`teleddns.spec`](teleddns.spec), [`debian/`](debian), and the GitHub Actions
workflows. See [`PRD.md`](PRD.md) for the full milestone plan.

## Build

```sh
go build -o teleddns ./cmd/teleddns
```

Requires Go 1.26+.

## Usage

```sh
teleddns -c ./teleddns.yaml      # run as a daemon, log netlink events
teleddns -c ./teleddns.yaml -o   # one-shot: dump current state and exit
teleddns -V                      # print version
```

| Flag | Description |
|------|-------------|
| `-c`, `--config <FILE>` | config file path (default `/etc/teleddns/teleddns.yaml`) |
| `-o`, `--oneshot` | dump current state once and exit |
| `-v`, `--verbose` | increase log verbosity (repeatable) |
| `-q`, `--quiet` | decrease log verbosity (repeatable) |
| `-V`, `--version` | print version and exit |

The initial state dump runs unprivileged. Subscribing to rtnetlink multicast
groups for live notifications may require `CAP_NET_ADMIN` or root, depending on
the kernel.

`SIGINT`, `SIGTERM` and `SIGHUP` all trigger a graceful shutdown. (The Rust
client only honored `SIGHUP`.)

## Configuration

The on-disk format is identical to the Rust client's. See
[`teleddns.yaml.sample`](teleddns.yaml.sample) for the annotated schema.

## Implementation notes

### Netlink library

`teleddns-go` uses [`github.com/vishvananda/netlink`](https://github.com/vishvananda/netlink).
It exposes `LinkSubscribe` / `AddrSubscribe` for rtnetlink multicast
notifications and `LinkList` / `AddrList` for the startup dump as typed Go
values, which keeps the client code small. It is pure Go for the rtnetlink
paths used here (no cgo). The alternative considered was
`github.com/jsimonetti/rtnetlink/v2`; see `PRD.md` "Open questions".

The internal package is `internal/watch` rather than `internal/netlink` to
avoid a name collision with the imported `netlink` package.

## Layout

```
cmd/teleddns/main.go       CLI, signal handling, wiring
internal/config/           teleddns.yaml loader
internal/applog/           slog setup, verbosity mapping
internal/watch/            rtnetlink observer: startup dump + event stream
internal/observe/          pretty-print events to the log
```

## License

GPLv3. See [`LICENSE`](LICENSE).

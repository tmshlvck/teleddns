// Package applog configures the process-wide slog logger.
//
// Output is a text handler ("key=value" structured form) so an operator
// reading `journalctl -u teleddns` sees both a human summary (the message)
// and the structured fields the PRD asks for.
package applog

import (
	"log/slog"
	"os"
)

// Setup installs a TextHandler at the given level as the default slog logger
// and returns it. The timestamp is dropped because systemd/journald already
// stamps each line.
func Setup(level slog.Level) *slog.Logger {
	h := slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{
		Level: level,
		ReplaceAttr: func(groups []string, a slog.Attr) slog.Attr {
			if len(groups) == 0 && a.Key == slog.TimeKey {
				return slog.Attr{}
			}
			return a
		},
	})
	logger := slog.New(h)
	slog.SetDefault(logger)
	return logger
}

// LevelFor maps a verbosity offset to an slog level. The base is Info; each
// step from -v lowers the threshold (more output), each -q raises it. Passing
// debug=true pins the level to Debug regardless of the offset, mirroring the
// Rust client's `debug: true` config override.
func LevelFor(verbosity int, debug bool) slog.Level {
	if debug {
		return slog.LevelDebug
	}
	// slog levels are 4 apart: Debug -4, Info 0, Warn 4, Error 8.
	lvl := slog.Level(-verbosity * 4)
	if lvl < slog.LevelDebug {
		lvl = slog.LevelDebug
	}
	if lvl > slog.LevelError {
		lvl = slog.LevelError
	}
	return lvl
}

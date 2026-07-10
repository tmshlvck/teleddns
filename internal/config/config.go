// Package config loads the teleddns YAML configuration file.
//
// The schema is identical to the Rust client's so existing deployments can
// switch binaries without re-configuring. Milestone 1 only consults `debug`
// and `interfaces`, but the whole file is parsed and validated up front.
package config

import (
	"bytes"
	"fmt"
	"os"
	"time"

	"gopkg.in/yaml.v3"
)

// Interval defaults, applied when the key is absent from the config file.
const (
	// DefaultResyncInterval is how often the client re-pushes the current
	// address even when nothing changed, so a record the server lost (zone
	// reset, restore from backup) is restored without operator action.
	DefaultResyncInterval = 24 * time.Hour
	// DefaultRedumpInterval is how often the client rebuilds its address map
	// from a full netlink dump, reconciling state that a dropped multicast
	// message would otherwise leave stale until the next unrelated event.
	DefaultRedumpInterval = 15 * time.Minute
	// minInterval is the floor for both. It matches daemon.MinUpdateInterval,
	// which is the shortest period at which the worker acts on anything;
	// duplicating it here avoids a config -> daemon import cycle.
	minInterval = 30 * time.Second
)

// Hook is a single post-change action. A hook may carry either field, or both.
type Hook struct {
	Shell          string `yaml:"shell"`
	NftSetsOutfile string `yaml:"nft_sets_outfile"`
}

// Config mirrors teleddns.yaml. Debug is a pointer so an absent key (nil) is
// distinguishable from an explicit `debug: false`, matching the Rust
// Option<bool> semantics. The interval fields are pointers for the same reason:
// absent means "use the default", an explicit `0s` means "disabled". Read them
// through Resync and Redump rather than directly.
type Config struct {
	Debug             *bool          `yaml:"debug"`
	DDNSURL           string         `yaml:"ddns_url"`
	Hostname          string         `yaml:"hostname"`
	EnableIPv6        bool           `yaml:"enable_ipv6"`
	EnableIPv4        bool           `yaml:"enable_ipv4"`
	ReportIPv4Private bool           `yaml:"report_ipv4_private"`
	ReportIPv6ULA     bool           `yaml:"report_ipv6_ula"`
	ResyncInterval    *time.Duration `yaml:"resync_interval"`
	RedumpInterval    *time.Duration `yaml:"redump_interval"`
	Interfaces        []string       `yaml:"interfaces"`
	Hooks             []Hook         `yaml:"hooks"`
}

// Resync returns the keepalive interval, or 0 when the keepalive is disabled.
func (c *Config) Resync() time.Duration {
	if c.ResyncInterval == nil {
		return DefaultResyncInterval
	}
	return *c.ResyncInterval
}

// Redump returns the full-dump reconcile interval, or 0 when it is disabled.
func (c *Config) Redump() time.Duration {
	if c.RedumpInterval == nil {
		return DefaultRedumpInterval
	}
	return *c.RedumpInterval
}

// Load reads and parses the configuration file at path.
func Load(path string) (*Config, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("open config %s: %w", path, err)
	}

	var cfg Config
	dec := yaml.NewDecoder(bytes.NewReader(data))
	dec.KnownFields(true)
	if err := dec.Decode(&cfg); err != nil {
		return nil, fmt.Errorf("parse config %s: %w", path, err)
	}

	if err := cfg.validate(); err != nil {
		return nil, fmt.Errorf("invalid config %s: %w", path, err)
	}
	return &cfg, nil
}

// DebugEnabled reports whether the config forces debug logging.
func (c *Config) DebugEnabled() bool {
	return c.Debug != nil && *c.Debug
}

func (c *Config) validate() error {
	if c.DDNSURL == "" {
		return fmt.Errorf("ddns_url is required")
	}
	if c.Hostname == "" {
		return fmt.Errorf("hostname is required")
	}
	if len(c.Interfaces) == 0 {
		return fmt.Errorf("interfaces list is empty")
	}
	// YAML durations are strings ("24h", "15m"), so disabling must be spelled
	// `0s`: a bare `0` is an int and fails to decode into time.Duration.
	for _, iv := range []struct {
		key string
		d   time.Duration
	}{
		{"resync_interval", c.Resync()},
		{"redump_interval", c.Redump()},
	} {
		if iv.d < 0 {
			return fmt.Errorf("%s must not be negative", iv.key)
		}
		if iv.d > 0 && iv.d < minInterval {
			return fmt.Errorf("%s must be 0s (disabled) or at least %s, got %s", iv.key, minInterval, iv.d)
		}
	}
	return nil
}

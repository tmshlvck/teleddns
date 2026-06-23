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

	"gopkg.in/yaml.v3"
)

// Hook is a single post-change action. A hook may carry either field, or both.
type Hook struct {
	Shell          string `yaml:"shell"`
	NftSetsOutfile string `yaml:"nft_sets_outfile"`
}

// Config mirrors teleddns.yaml. Debug is a pointer so an absent key (nil) is
// distinguishable from an explicit `debug: false`, matching the Rust
// Option<bool> semantics.
type Config struct {
	Debug             *bool    `yaml:"debug"`
	DDNSURL           string   `yaml:"ddns_url"`
	Hostname          string   `yaml:"hostname"`
	EnableIPv6        bool     `yaml:"enable_ipv6"`
	EnableIPv4        bool     `yaml:"enable_ipv4"`
	ReportIPv4Private bool     `yaml:"report_ipv4_private"`
	ReportIPv6ULA     bool     `yaml:"report_ipv6_ula"`
	Interfaces        []string `yaml:"interfaces"`
	Hooks             []Hook   `yaml:"hooks"`
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
	return nil
}

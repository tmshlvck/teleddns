package config

import (
	"os"
	"path/filepath"
	"testing"
)

func writeTemp(t *testing.T, content string) string {
	t.Helper()
	path := filepath.Join(t.TempDir(), "teleddns.yaml")
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatalf("write temp config: %v", err)
	}
	return path
}

func TestLoadValid(t *testing.T) {
	path := writeTemp(t, `
debug: true
ddns_url: 'https://u:p@host/ddns/update'
hostname: 'h.example.com'
enable_ipv6: true
enable_ipv4: false
interfaces:
  - '*'
  - '-virbr0'
hooks:
  - nft_sets_outfile: '/etc/nftables.d/00.rules'
    shell: 'nft -f /etc/nftables.conf'
`)
	cfg, err := Load(path)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if !cfg.DebugEnabled() {
		t.Errorf("DebugEnabled() = false, want true")
	}
	if got := cfg.Interfaces; len(got) != 2 || got[1] != "-virbr0" {
		t.Errorf("Interfaces = %v, want [* -virbr0]", got)
	}
	if len(cfg.Hooks) != 1 || cfg.Hooks[0].NftSetsOutfile == "" {
		t.Errorf("Hooks = %v, want one hook with nft_sets_outfile", cfg.Hooks)
	}
}

func TestDebugDefaultsToNil(t *testing.T) {
	path := writeTemp(t, `
ddns_url: 'https://u:p@host/ddns/update'
hostname: 'h.example.com'
enable_ipv6: true
enable_ipv4: false
interfaces: ['*']
`)
	cfg, err := Load(path)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if cfg.Debug != nil {
		t.Errorf("Debug = %v, want nil when key absent", *cfg.Debug)
	}
	if cfg.DebugEnabled() {
		t.Errorf("DebugEnabled() = true, want false")
	}
	// report_* keys default to false when absent.
	if cfg.ReportIPv4Private || cfg.ReportIPv6ULA {
		t.Errorf("report_* defaulted true, want false")
	}
}

func TestLoadRejectsMissingRequired(t *testing.T) {
	path := writeTemp(t, `
hostname: 'h.example.com'
enable_ipv6: true
enable_ipv4: false
interfaces: ['*']
`)
	if _, err := Load(path); err == nil {
		t.Fatal("Load accepted config missing ddns_url, want error")
	}
}

func TestLoadRejectsUnknownField(t *testing.T) {
	path := writeTemp(t, `
ddns_url: 'https://u:p@host/ddns/update'
hostname: 'h.example.com'
enable_ipv6: true
enable_ipv4: false
interfaces: ['*']
typpo_field: true
`)
	if _, err := Load(path); err == nil {
		t.Fatal("Load accepted config with unknown field, want error")
	}
}

func TestLoadMissingFile(t *testing.T) {
	if _, err := Load("/nonexistent/teleddns.yaml"); err == nil {
		t.Fatal("Load accepted missing file, want error")
	}
}

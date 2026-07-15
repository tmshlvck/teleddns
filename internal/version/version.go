// Package version reports the teleddns build version as either a release number
// ("1.2.3") or, for an unstamped development build, "dev-<short git hash>".
package version

import "runtime/debug"

// version is the release version. It is empty for a plain `go build` during
// development, and is set at release time and by the RPM/deb packaging with:
//
//	-ldflags "-X github.com/tmshlvck/teleddns/internal/version.version=1.2.3"
var version = ""

// String returns the stamped release version, or "dev-<short git hash>" for an
// unstamped build (plain "dev" when built with no VCS metadata, e.g. from a
// release tarball with no .git).
func String() string {
	if version != "" {
		return version
	}
	if bi, ok := debug.ReadBuildInfo(); ok {
		for _, s := range bi.Settings {
			if s.Key == "vcs.revision" {
				if len(s.Value) > 12 {
					return "dev-" + s.Value[:12]
				}
				return "dev-" + s.Value
			}
		}
	}
	return "dev"
}

// UserAgent is the HTTP User-Agent the DDNS client sends, e.g. "teleddns/1.2.3"
// or "teleddns/dev-a1b2c3d4e5f6".
func UserAgent() string { return "teleddns/" + String() }

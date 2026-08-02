package update

import (
	"crypto/sha256"
	"encoding/hex"
	"strings"
	"testing"
)

func TestCompareVersions(t *testing.T) {
	tests := map[string]struct {
		a, b string
		want int
	}{
		"patch newer":       {a: "1.2.3", b: "1.2.2", want: 1},
		"minor older":       {a: "1.1.9", b: "1.2.0", want: -1},
		"major newer":       {a: "2.0.0", b: "1.9.9", want: 1},
		"equal":             {a: "1.2.3", b: "1.2.3", want: 0},
		"v prefix ignored":  {a: "v1.2.3", b: "1.2.3", want: 0},
		"release beats pre": {a: "1.2.3", b: "1.2.3-nightly.1", want: 1},
		"pre ordering":      {a: "1.2.3-nightly.2", b: "1.2.3-nightly.1", want: 1},
	}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			if got := CompareVersions(tc.a, tc.b); got != tc.want {
				t.Errorf("CompareVersions(%q, %q) = %d, want %d", tc.a, tc.b, got, tc.want)
			}
		})
	}
}

func TestIsNewer(t *testing.T) {
	tests := map[string]struct {
		current, latest string
		want            bool
	}{
		"upgrade":     {current: "1.0.0", latest: "1.1.0", want: true},
		"same":        {current: "1.1.0", latest: "1.1.0", want: false},
		"downgrade":   {current: "1.2.0", latest: "1.1.0", want: false},
		"dev never":   {current: "dev", latest: "9.9.9", want: false},
		"empty never": {current: "", latest: "9.9.9", want: false},
	}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			if got := IsNewer(tc.current, tc.latest); got != tc.want {
				t.Errorf("IsNewer(%q, %q) = %v, want %v", tc.current, tc.latest, got, tc.want)
			}
		})
	}
}

func TestVerifyChecksum(t *testing.T) {
	data := []byte("release payload")
	digest := sha256.Sum256(data)
	good := hex.EncodeToString(digest[:]) + "  argmax_linux_amd64.tar.gz\n"
	if err := verifyChecksum(data, good, "argmax_linux_amd64.tar.gz"); err != nil {
		t.Errorf("valid checksum rejected: %v", err)
	}
	bad := strings.Repeat("0", 64) + "  argmax_linux_amd64.tar.gz\n"
	if err := verifyChecksum(data, bad, "argmax_linux_amd64.tar.gz"); err == nil {
		t.Error("mismatched checksum accepted")
	}
	if err := verifyChecksum(data, good, "argmax_darwin_arm64.tar.gz"); err == nil {
		t.Error("missing checksum entry accepted")
	}
}

package update

import (
	"archive/tar"
	"bytes"
	"compress/gzip"
	"crypto/sha256"
	"encoding/hex"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
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
	if err := verifyChecksum(digest, good, "argmax_linux_amd64.tar.gz"); err != nil {
		t.Errorf("valid checksum rejected: %v", err)
	}
	bad := strings.Repeat("0", 64) + "  argmax_linux_amd64.tar.gz\n"
	if err := verifyChecksum(digest, bad, "argmax_linux_amd64.tar.gz"); err == nil {
		t.Error("mismatched checksum accepted")
	}
	if err := verifyChecksum(digest, good, "argmax_darwin_arm64.tar.gz"); err == nil {
		t.Error("missing checksum entry accepted")
	}
}

func TestDownloadEnforcesBodyLimit(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/exact":
			_, _ = w.Write([]byte("1234"))
		case "/oversized":
			_, _ = w.Write([]byte("12345"))
		default:
			http.Error(w, "failure", http.StatusBadGateway)
		}
	}))
	defer server.Close()

	got, err := download(server.Client(), server.URL+"/exact", 4)
	if err != nil || string(got) != "1234" {
		t.Errorf("exact-limit download = %q, %v", got, err)
	}
	if _, err := download(server.Client(), server.URL+"/oversized", 4); err == nil {
		t.Error("oversized download succeeded")
	}
	if _, err := download(server.Client(), server.URL+"/failure", 4); err == nil {
		t.Error("non-200 download succeeded")
	}
}

func TestDownloadToTempStreamsAndCleansFailures(t *testing.T) {
	tempDir := t.TempDir()
	t.Setenv("TMPDIR", tempDir)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		_, _ = w.Write([]byte("payload"))
	}))
	defer server.Close()

	file, digest, err := downloadToTemp(server.Client(), server.URL, 7)
	if err != nil {
		t.Fatal(err)
	}
	got, err := io.ReadAll(file)
	if err != nil {
		t.Fatal(err)
	}
	wantDigest := sha256.Sum256([]byte("payload"))
	if string(got) != "payload" || digest != wantDigest {
		t.Errorf("download = %q, %x; want payload, %x", got, digest, wantDigest)
	}
	name := file.Name()
	if err := file.Close(); err != nil {
		t.Fatal(err)
	}
	if err := os.Remove(name); err != nil {
		t.Fatal(err)
	}

	if _, _, err := downloadToTemp(server.Client(), server.URL, 6); err == nil {
		t.Error("oversized temporary download succeeded")
	}
	entries, err := os.ReadDir(tempDir)
	if err != nil {
		t.Fatal(err)
	}
	if len(entries) != 0 {
		t.Errorf("failed download left temporary files: %v", entries)
	}
}

func TestReplaceBinaryStreamsArchiveEntry(t *testing.T) {
	dir := t.TempDir()
	destination := filepath.Join(dir, "argmax")
	if err := os.WriteFile(destination, []byte("old"), 0o755); err != nil {
		t.Fatal(err)
	}
	want := []byte{'n', 'e', 'w', 0, 0xff, '\n'}
	archive := makeArchive(t, "release/argmax", tar.TypeReg, want)
	if err := replaceBinary(bytes.NewReader(archive), destination, int64(len(want))); err != nil {
		t.Fatal(err)
	}
	got, err := os.ReadFile(destination)
	if err != nil {
		t.Fatal(err)
	}
	info, err := os.Stat(destination)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(got, want) {
		t.Errorf("replacement bytes = %v, want %v", got, want)
	}
	if info.Mode().Perm()&0o111 == 0 {
		t.Errorf("replacement mode = %v, want executable", info.Mode())
	}
}

func TestReplaceBinaryPreservesOriginalOnArchiveFailure(t *testing.T) {
	dir := t.TempDir()
	destination := filepath.Join(dir, "argmax")
	if err := os.WriteFile(destination, []byte("original"), 0o755); err != nil {
		t.Fatal(err)
	}
	archive := makeArchive(t, "release/argmax", tar.TypeReg, []byte("oversized"))
	if err := replaceBinary(bytes.NewReader(archive), destination, 4); err == nil {
		t.Fatal("oversized archive entry replaced the binary")
	}
	got, err := os.ReadFile(destination)
	if err != nil {
		t.Fatal(err)
	}
	if string(got) != "original" {
		t.Errorf("original binary changed to %q", got)
	}
	temps, err := filepath.Glob(filepath.Join(dir, ".argmax-update-*"))
	if err != nil {
		t.Fatal(err)
	}
	if len(temps) != 0 {
		t.Errorf("failed replacement left temporary files: %v", temps)
	}
}

func TestExtractBinaryRejectsMissingRegularEntry(t *testing.T) {
	archive := makeArchive(t, "release/argmax", tar.TypeSymlink, nil)
	if err := extractBinary(bytes.NewReader(archive), io.Discard, 100); err == nil {
		t.Error("archive without a regular argmax binary succeeded")
	}
	empty := makeArchive(t, "release/argmax", tar.TypeReg, nil)
	if err := extractBinary(bytes.NewReader(empty), io.Discard, 100); err == nil {
		t.Error("empty argmax binary succeeded")
	}
}

func makeArchive(t *testing.T, name string, typeflag byte, data []byte) []byte {
	t.Helper()
	var archive bytes.Buffer
	gz := gzip.NewWriter(&archive)
	tw := tar.NewWriter(gz)
	header := &tar.Header{Name: name, Mode: 0o755, Typeflag: typeflag}
	if typeflag == tar.TypeReg {
		header.Size = int64(len(data))
	} else if typeflag == tar.TypeSymlink {
		header.Linkname = "elsewhere"
	}
	if err := tw.WriteHeader(header); err != nil {
		t.Fatal(err)
	}
	if typeflag == tar.TypeReg {
		if _, err := tw.Write(data); err != nil {
			t.Fatal(err)
		}
	}
	if err := tw.Close(); err != nil {
		t.Fatal(err)
	}
	if err := gz.Close(); err != nil {
		t.Fatal(err)
	}
	return archive.Bytes()
}

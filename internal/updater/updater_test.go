package updater

import (
	"archive/tar"
	"bytes"
	"compress/gzip"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
)

// serveReleases starts a test release API and points ARGMAX_UPDATE_URL at
// it. The handler is mounted under "/releases"; extra handlers may be added
// to the returned mux.
func serveReleases(t *testing.T, releases []release) (*httptest.Server, *http.ServeMux) {
	t.Helper()
	mux := http.NewServeMux()
	mux.HandleFunc("/releases", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(releases)
	})
	srv := httptest.NewServer(mux)
	t.Cleanup(srv.Close)
	t.Setenv("ARGMAX_UPDATE_URL", srv.URL)
	return srv, mux
}

func TestCheckStableSkipsPrereleases(t *testing.T) {
	serveReleases(t, []release{
		{TagName: "v1.2.0", Prerelease: false},
		{TagName: "v1.3.0-nightly.1", Prerelease: true},
		{TagName: "v1.2.4-nightly.2", Prerelease: true},
	})
	info, has, err := Check(context.Background(), "stable", "1.1.0")
	if err != nil {
		t.Fatal(err)
	}
	if info.Version != "1.2.0" {
		t.Fatalf("stable must ignore prereleases: got %q", info.Version)
	}
	if !has {
		t.Fatal("1.1.0 -> 1.2.0 must report an update")
	}

	_, has, err = Check(context.Background(), "stable", "1.2.0")
	if err != nil {
		t.Fatal(err)
	}
	if has {
		t.Fatal("current == latest must not report an update")
	}
}

func TestCheckNightlyPicksNewestPrerelease(t *testing.T) {
	srv, _ := serveReleases(t, []release{
		{TagName: "v1.2.0", Prerelease: false},
		{TagName: "v1.2.4-nightly.2", Prerelease: true},
		{TagName: "v1.3.0-nightly.1", Prerelease: true},
	})
	info, has, err := Check(context.Background(), "nightly", "1.2.0")
	if err != nil {
		t.Fatal(err)
	}
	if info.Version != "1.3.0-nightly.1" {
		t.Fatalf("nightly must pick the newest prerelease: got %q", info.Version)
	}
	if !has {
		t.Fatal("1.2.0 -> 1.3.0-nightly.1 must report an update")
	}
	wantURL := srv.URL + "/releases/tag/v1.3.0-nightly.1"
	if info.URL != wantURL {
		t.Fatalf("URL fallback: got %q, want %q", info.URL, wantURL)
	}
}

func TestCheckNightlyFallsBackToReleases(t *testing.T) {
	serveReleases(t, []release{{TagName: "v1.2.0", Prerelease: false}})
	info, has, err := Check(context.Background(), "nightly", "1.1.0")
	if err != nil {
		t.Fatal(err)
	}
	if info.Version != "1.2.0" || !has {
		t.Fatalf("nightly must fall back to full releases: got %+v has=%v", info, has)
	}
}

func TestCheckDevNeverReportsUpdate(t *testing.T) {
	// Any request would fail: the URL is unroutable.
	t.Setenv("ARGMAX_UPDATE_URL", "http://127.0.0.1:1")
	for _, current := range []string{"dev", ""} {
		info, has, err := Check(context.Background(), "stable", current)
		if err != nil {
			t.Fatalf("current %q: %v", current, err)
		}
		if has || info.Version != "" {
			t.Fatalf("current %q must never report an update: %+v has=%v", current, info, has)
		}
	}
}

func TestCheckHTTPError(t *testing.T) {
	mux := http.NewServeMux()
	mux.HandleFunc("/releases", func(w http.ResponseWriter, r *http.Request) {
		http.Error(w, "boom", http.StatusInternalServerError)
	})
	srv := httptest.NewServer(mux)
	t.Cleanup(srv.Close)
	t.Setenv("ARGMAX_UPDATE_URL", srv.URL)

	_, _, err := Check(context.Background(), "stable", "1.0.0")
	if err == nil || !strings.Contains(err.Error(), "500") {
		t.Fatalf("expected status error, got %v", err)
	}
}

func TestCheckCanceledContext(t *testing.T) {
	serveReleases(t, []release{{TagName: "v1.2.0"}})
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if _, _, err := Check(ctx, "stable", "1.0.0"); err == nil {
		t.Fatal("canceled context must error")
	}
}

func TestNewer(t *testing.T) {
	cases := []struct {
		current, latest string
		want            bool
	}{
		{"1.2.3", "1.2.4", true},
		{"1.2.4", "1.2.3", false},
		{"1.2.3", "1.2.3", false},
		{"v1.2.3", "v1.2.4", true},
		{"1.9.0", "1.10.0", true},
		{"1.10.0", "1.9.0", false},
		{"1.2.3-nightly.1", "1.2.3", true},
		{"1.2.3", "1.2.3-nightly.1", false},
		{"1.2.3-nightly.1", "1.2.3-nightly.2", true},
		{"1.2.3-nightly.2", "1.2.3-nightly.10", true},
		{"1.2.3-nightly.1", "1.2.3-nightly.1", false},
		{"1.2.3-nightly.1", "1.2.3-nightly.1.0", true},
		{"1.2", "1.2.1", true},
		{"1.2.1", "1.2", false},
		{"1.2.3+build", "1.2.4", true},
		{"dev", "1.0.0", false},
		{"1.0.0", "garbage", false},
	}
	for _, c := range cases {
		if got := Newer(c.current, c.latest); got != c.want {
			t.Errorf("Newer(%q, %q) = %v, want %v", c.current, c.latest, got, c.want)
		}
	}
}

// makeArchive builds a .tar.gz release archive holding the binary entry
// plus a decoy file.
func makeArchive(t *testing.T, binary []byte) []byte {
	t.Helper()
	var buf bytes.Buffer
	gz := gzip.NewWriter(&buf)
	tw := tar.NewWriter(gz)
	for name, data := range map[string][]byte{"README.md": []byte("hi\n"), "argmax": binary} {
		if err := tw.WriteHeader(&tar.Header{
			Name:     name,
			Mode:     0o755,
			Size:     int64(len(data)),
			Typeflag: tar.TypeReg,
		}); err != nil {
			t.Fatal(err)
		}
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
	return buf.Bytes()
}

// updateServer serves a one-release API for version with the given archive.
// With corruptSums false the checksums file carries the archive's real
// digest; with true it carries a bogus one (both under the correct asset
// name).
func updateServer(t *testing.T, version string, archive []byte, corruptSums bool) {
	t.Helper()
	assetName := fmt.Sprintf("argmax_%s_%s_%s.tar.gz", version, runtime.GOOS, runtime.GOARCH)

	digest := strings.Repeat("0", 64)
	if !corruptSums {
		sum := sha256.Sum256(archive)
		digest = hex.EncodeToString(sum[:])
	}
	sums := []byte(digest + "  " + assetName + "\n")

	mux := http.NewServeMux()
	srv := httptest.NewServer(mux)
	t.Cleanup(srv.Close)
	t.Setenv("ARGMAX_UPDATE_URL", srv.URL)

	mux.HandleFunc("/releases", func(w http.ResponseWriter, r *http.Request) {
		_ = json.NewEncoder(w).Encode([]release{{
			TagName: "v" + version,
			Assets: []asset{
				{Name: assetName, URL: srv.URL + "/dl/" + assetName},
				{Name: checksumsName, URL: srv.URL + "/dl/" + checksumsName},
			},
		}})
	})
	mux.HandleFunc("/dl/"+assetName, func(w http.ResponseWriter, r *http.Request) {
		_, _ = w.Write(archive)
	})
	mux.HandleFunc("/dl/"+checksumsName, func(w http.ResponseWriter, r *http.Request) {
		_, _ = w.Write(sums)
	})
}

func TestSelfUpdateGoodChecksum(t *testing.T) {
	newBinary := []byte("#!/bin/sh\necho new\n")
	updateServer(t, "1.2.0", makeArchive(t, newBinary), false)

	target := filepath.Join(t.TempDir(), "argmax")
	oldBinary := []byte("#!/bin/sh\necho old\n")
	if err := os.WriteFile(target, oldBinary, 0o755); err != nil {
		t.Fatal(err)
	}

	var out bytes.Buffer
	if err := selfUpdate(context.Background(), "stable", "1.1.0", target, &out); err != nil {
		t.Fatal(err)
	}
	got, err := os.ReadFile(target)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(got, newBinary) {
		t.Fatalf("binary not swapped: got %q", got)
	}
	info, err := os.Stat(target)
	if err != nil {
		t.Fatal(err)
	}
	if info.Mode().Perm() != 0o755 {
		t.Fatalf("mode = %o, want 755", info.Mode().Perm())
	}
	text := out.String()
	for _, want := range []string{"current version: 1.1.0", "latest version: 1.2.0", "updated to 1.2.0"} {
		if !strings.Contains(text, want) {
			t.Errorf("output missing %q:\n%s", want, text)
		}
	}
}

func TestSelfUpdateBadChecksumLeavesBinaryIntact(t *testing.T) {
	archive := makeArchive(t, []byte("#!/bin/sh\necho new\n"))
	updateServer(t, "1.2.0", archive, true)

	target := filepath.Join(t.TempDir(), "argmax")
	oldBinary := []byte("#!/bin/sh\necho old\n")
	if err := os.WriteFile(target, oldBinary, 0o755); err != nil {
		t.Fatal(err)
	}

	var out bytes.Buffer
	err := selfUpdate(context.Background(), "stable", "1.1.0", target, &out)
	if err == nil || !strings.Contains(err.Error(), "checksum mismatch") {
		t.Fatalf("expected checksum mismatch, got %v", err)
	}
	got, readErr := os.ReadFile(target)
	if readErr != nil {
		t.Fatal(readErr)
	}
	if !bytes.Equal(got, oldBinary) {
		t.Fatalf("failed update must leave the binary intact: got %q", got)
	}
}

func TestSelfUpdateAlreadyCurrent(t *testing.T) {
	updateServer(t, "1.2.0", nil, false) // asset handlers must not be hit

	target := filepath.Join(t.TempDir(), "argmax")
	oldBinary := []byte("old")
	if err := os.WriteFile(target, oldBinary, 0o755); err != nil {
		t.Fatal(err)
	}

	var out bytes.Buffer
	if err := selfUpdate(context.Background(), "stable", "1.2.0", target, &out); err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(out.String(), "already up to date") {
		t.Fatalf("expected already-current report, got:\n%s", out.String())
	}
	got, err := os.ReadFile(target)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(got, oldBinary) {
		t.Fatal("already-current update must not touch the binary")
	}
}

func TestSelfUpdateDevBuild(t *testing.T) {
	if err := selfUpdate(context.Background(), "stable", "dev", filepath.Join(t.TempDir(), "argmax"), &bytes.Buffer{}); err == nil {
		t.Fatal("dev builds must refuse self-update")
	}
}

func TestExtractBinary(t *testing.T) {
	want := []byte("#!/bin/sh\necho hi\n")
	data, err := extractBinary(makeArchive(t, want))
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(data, want) {
		t.Fatalf("got %q", data)
	}

	// An archive without the binary entry errors.
	var buf bytes.Buffer
	gz := gzip.NewWriter(&buf)
	tw := tar.NewWriter(gz)
	if err := tw.WriteHeader(&tar.Header{Name: "README.md", Mode: 0o644, Size: 2, Typeflag: tar.TypeReg}); err != nil {
		t.Fatal(err)
	}
	if _, err := tw.Write([]byte("hi")); err != nil {
		t.Fatal(err)
	}
	if err := tw.Close(); err != nil {
		t.Fatal(err)
	}
	if err := gz.Close(); err != nil {
		t.Fatal(err)
	}
	if _, err := extractBinary(buf.Bytes()); err == nil {
		t.Fatal("archive without argmax entry must error")
	}
}

func TestReplaceBinary(t *testing.T) {
	dir := t.TempDir()
	target := filepath.Join(dir, "argmax")
	if err := os.WriteFile(target, []byte("old"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := replaceBinary(target, []byte("new")); err != nil {
		t.Fatal(err)
	}
	got, err := os.ReadFile(target)
	if err != nil {
		t.Fatal(err)
	}
	if string(got) != "new" {
		t.Fatalf("got %q", got)
	}
	info, err := os.Stat(target)
	if err != nil {
		t.Fatal(err)
	}
	if info.Mode().Perm() != 0o755 {
		t.Fatalf("mode = %o, want 755", info.Mode().Perm())
	}

	// A target in a missing directory errors and creates nothing.
	missing := filepath.Join(dir, "nope", "argmax")
	if err := replaceBinary(missing, []byte("x")); err == nil {
		t.Fatal("expected error for missing directory")
	}
	if _, err := os.Stat(missing); !os.IsNotExist(err) {
		t.Fatalf("unexpected file at %s", missing)
	}
}

func TestChecksumFor(t *testing.T) {
	sums := []byte("aaa  argmax_1.2.0_linux_amd64.tar.gz\nbbb  *argmax_1.2.0_darwin_arm64.tar.gz\n")
	if got, err := checksumFor(sums, "argmax_1.2.0_linux_amd64.tar.gz"); err != nil || got != "aaa" {
		t.Fatalf("got %q, %v", got, err)
	}
	if got, err := checksumFor(sums, "argmax_1.2.0_darwin_arm64.tar.gz"); err != nil || got != "bbb" {
		t.Fatalf("binary-mode marker: got %q, %v", got, err)
	}
	if _, err := checksumFor(sums, "missing.tar.gz"); err == nil {
		t.Fatal("missing entry must error")
	}
}

// The goreleaser pipeline publishes unversioned assets; the updater must
// accept them (and prefer them).
func TestAssetNamesPrefersUnversioned(t *testing.T) {
	want := fmt.Sprintf("argmax_%s_%s.tar.gz", runtime.GOOS, runtime.GOARCH)
	names := assetNames("v1.2.0", "1.2.0")
	if len(names) == 0 || names[0] != want {
		t.Fatalf("assetNames[0] = %v, want %q first", names, want)
	}
	found := findAsset(&release{Assets: []asset{{Name: want}}}, names...)
	if found == nil || found.Name != want {
		t.Fatalf("findAsset must match the unversioned asset, got %+v", found)
	}
}

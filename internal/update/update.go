// Package update implements release checks and verified self-update for
// the stable and nightly channels.
package update

import (
	"archive/tar"
	"compress/gzip"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"time"
)

const (
	releasesURL    = "https://api.github.com/repos/rselbach/argmax/releases"
	networkTimeout = 5 * time.Second
)

// Release describes one published release.
type Release struct {
	Version string
	Assets  map[string]string // file name -> download URL
}

type apiRelease struct {
	TagName    string `json:"tag_name"`
	Prerelease bool   `json:"prerelease"`
	Assets     []struct {
		Name string `json:"name"`
		URL  string `json:"browser_download_url"`
	} `json:"assets"`
}

// Latest returns the newest release for the channel, or ok=false when the
// current channel has no matching release.
func Latest(channel string) (Release, bool, error) {
	client := &http.Client{Timeout: networkTimeout}
	resp, err := client.Get(releasesURL)
	if err != nil {
		return Release{}, false, fmt.Errorf("query releases: %w", err)
	}
	defer func() { _ = resp.Body.Close() }()
	if resp.StatusCode != http.StatusOK {
		return Release{}, false, fmt.Errorf("release API returned HTTP %d", resp.StatusCode)
	}
	data, err := readBounded(resp.Body, 4<<20)
	if err != nil {
		return Release{}, false, fmt.Errorf("read releases: %w", err)
	}
	var releases []apiRelease
	if err := json.Unmarshal(data, &releases); err != nil {
		return Release{}, false, fmt.Errorf("decode releases: %w", err)
	}
	var best *apiRelease
	for i := range releases {
		r := &releases[i]
		nightly := r.Prerelease || strings.Contains(r.TagName, "nightly")
		if (channel == "nightly") != nightly {
			continue
		}
		if best == nil || CompareVersions(r.TagName, best.TagName) > 0 {
			best = r
		}
	}
	if best == nil {
		return Release{}, false, nil
	}
	out := Release{Version: strings.TrimPrefix(best.TagName, "v"), Assets: map[string]string{}}
	for _, a := range best.Assets {
		out.Assets[a.Name] = a.URL
	}
	return out, true, nil
}

// CompareVersions compares semantic versions; a release version is newer
// than its prereleases.
func CompareVersions(a, b string) int {
	pa, prea := parseVersion(a)
	pb, preb := parseVersion(b)
	for i := range 3 {
		if pa[i] != pb[i] {
			if pa[i] > pb[i] {
				return 1
			}
			return -1
		}
	}
	switch {
	case prea == preb:
		return 0
	case prea == "":
		return 1
	case preb == "":
		return -1
	case prea > preb:
		return 1
	default:
		return -1
	}
}

func parseVersion(v string) ([3]int, string) {
	v = strings.TrimPrefix(strings.TrimSpace(v), "v")
	var pre string
	if i := strings.IndexAny(v, "-+"); i >= 0 {
		pre = v[i+1:]
		v = v[:i]
	}
	var parts [3]int
	for i, p := range strings.SplitN(v, ".", 3) {
		if i >= 3 {
			break
		}
		n, err := strconv.Atoi(p)
		if err != nil {
			break
		}
		parts[i] = n
	}
	return parts, pre
}

// IsNewer reports whether latest is newer than current. Development
// builds never update.
func IsNewer(current, latest string) bool {
	if current == "dev" || current == "" {
		return false
	}
	return CompareVersions(latest, current) > 0
}

// Apply downloads the release asset for this OS/architecture, verifies
// its checksum, and atomically replaces the current binary. Any failure
// leaves the executable intact.
func Apply(rel Release) error {
	assetName := fmt.Sprintf("argmax_%s_%s.tar.gz", runtime.GOOS, runtime.GOARCH)
	assetURL, ok := rel.Assets[assetName]
	if !ok {
		return fmt.Errorf("release %s has no asset %s", rel.Version, assetName)
	}
	sumsURL, ok := rel.Assets["checksums.txt"]
	if !ok {
		return fmt.Errorf("release %s has no checksums.txt", rel.Version)
	}
	client := &http.Client{Timeout: 5 * time.Minute}

	archive, err := download(client, assetURL, 200<<20)
	if err != nil {
		return fmt.Errorf("download %s: %w", assetName, err)
	}
	sums, err := download(client, sumsURL, 1<<20)
	if err != nil {
		return fmt.Errorf("download checksums: %w", err)
	}
	if err := verifyChecksum(archive, string(sums), assetName); err != nil {
		return err
	}
	binary, err := extractBinary(archive)
	if err != nil {
		return err
	}

	self, err := os.Executable()
	if err != nil {
		return fmt.Errorf("resolve current binary: %w", err)
	}
	self, err = filepath.EvalSymlinks(self)
	if err != nil {
		return fmt.Errorf("resolve current binary: %w", err)
	}
	tmp, err := os.CreateTemp(filepath.Dir(self), ".argmax-update-*")
	if err != nil {
		return fmt.Errorf("stage update: %w", err)
	}
	defer func() { _ = os.Remove(tmp.Name()) }()
	if _, err := tmp.Write(binary); err != nil {
		_ = tmp.Close()
		return fmt.Errorf("write update: %w", err)
	}
	if err := tmp.Chmod(0o755); err != nil {
		_ = tmp.Close()
		return fmt.Errorf("mark update executable: %w", err)
	}
	if err := tmp.Close(); err != nil {
		return fmt.Errorf("finish staging update: %w", err)
	}
	if err := os.Rename(tmp.Name(), self); err != nil {
		return fmt.Errorf("replace binary: %w", err)
	}
	return nil
}

func download(client *http.Client, url string, limit int64) ([]byte, error) {
	resp, err := client.Get(url)
	if err != nil {
		return nil, err
	}
	defer func() { _ = resp.Body.Close() }()
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("HTTP %d", resp.StatusCode)
	}
	return readBounded(resp.Body, limit)
}

func readBounded(reader io.Reader, limit int64) ([]byte, error) {
	data, err := io.ReadAll(io.LimitReader(reader, limit+1))
	if err != nil {
		return nil, err
	}
	if int64(len(data)) > limit {
		return nil, fmt.Errorf("response exceeds %d-byte limit", limit)
	}
	return data, nil
}

func verifyChecksum(data []byte, sums, assetName string) error {
	digest := sha256.Sum256(data)
	want := hex.EncodeToString(digest[:])
	for _, ln := range strings.Split(sums, "\n") {
		fields := strings.Fields(ln)
		if len(fields) != 2 {
			continue
		}
		if strings.TrimPrefix(fields[1], "*") == assetName {
			if strings.EqualFold(fields[0], want) {
				return nil
			}
			return fmt.Errorf("checksum mismatch for %s", assetName)
		}
	}
	return fmt.Errorf("no checksum entry for %s", assetName)
}

func extractBinary(archive []byte) ([]byte, error) {
	gz, err := gzip.NewReader(strings.NewReader(string(archive)))
	if err != nil {
		return nil, fmt.Errorf("open archive: %w", err)
	}
	defer func() { _ = gz.Close() }()
	tr := tar.NewReader(gz)
	for {
		hdr, err := tr.Next()
		if err == io.EOF {
			return nil, fmt.Errorf("archive contains no argmax binary")
		}
		if err != nil {
			return nil, fmt.Errorf("read archive: %w", err)
		}
		if filepath.Base(hdr.Name) == "argmax" && hdr.Typeflag == tar.TypeReg {
			return io.ReadAll(io.LimitReader(tr, 500<<20))
		}
	}
}

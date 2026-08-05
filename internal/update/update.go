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
	"strings"
	"time"

	"golang.org/x/mod/semver"
)

const (
	releasesURL    = "https://api.github.com/repos/rselbach/argmax/releases"
	networkTimeout = 5 * time.Second
	archiveLimit   = 200 << 20
	checksumsLimit = 1 << 20
	binaryLimit    = 500 << 20
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
	return semver.Compare(normalizeVersion(a), normalizeVersion(b))
}

func normalizeVersion(version string) string {
	version = strings.TrimSpace(version)
	if !strings.HasPrefix(version, "v") {
		version = "v" + version
	}
	return version
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

	self, err := os.Executable()
	if err != nil {
		return fmt.Errorf("resolve current binary: %w", err)
	}
	self, err = filepath.EvalSymlinks(self)
	if err != nil {
		return fmt.Errorf("resolve current binary: %w", err)
	}

	archive, digest, err := downloadToTemp(client, assetURL, archiveLimit)
	if err != nil {
		return fmt.Errorf("download %s: %w", assetName, err)
	}
	defer func() {
		_ = archive.Close()
		_ = os.Remove(archive.Name())
	}()
	sums, err := download(client, sumsURL, checksumsLimit)
	if err != nil {
		return fmt.Errorf("download checksums: %w", err)
	}
	if err := verifyChecksum(digest, string(sums), assetName); err != nil {
		return err
	}
	return replaceBinary(archive, self, binaryLimit)
}

func replaceBinary(archive io.Reader, destination string, limit int64) error {
	tmp, err := os.CreateTemp(filepath.Dir(destination), ".argmax-update-*")
	if err != nil {
		return fmt.Errorf("stage update: %w", err)
	}
	defer func() { _ = os.Remove(tmp.Name()) }()
	if err := extractBinary(archive, tmp, limit); err != nil {
		_ = tmp.Close()
		return err
	}
	if err := tmp.Chmod(0o755); err != nil {
		_ = tmp.Close()
		return fmt.Errorf("mark update executable: %w", err)
	}
	if err := tmp.Sync(); err != nil {
		_ = tmp.Close()
		return fmt.Errorf("sync staged update: %w", err)
	}
	if err := tmp.Close(); err != nil {
		return fmt.Errorf("finish staging update: %w", err)
	}
	if err := os.Rename(tmp.Name(), destination); err != nil {
		return fmt.Errorf("replace binary: %w", err)
	}
	return nil
}

func downloadToTemp(client *http.Client, url string, limit int64) (*os.File, [sha256.Size]byte, error) {
	var digest [sha256.Size]byte
	tmp, err := os.CreateTemp("", ".argmax-download-*")
	if err != nil {
		return nil, digest, err
	}
	keep := false
	defer func() {
		if !keep {
			_ = tmp.Close()
			_ = os.Remove(tmp.Name())
		}
	}()

	resp, err := client.Get(url)
	if err != nil {
		return nil, digest, err
	}
	defer func() { _ = resp.Body.Close() }()
	if resp.StatusCode != http.StatusOK {
		return nil, digest, fmt.Errorf("HTTP %d", resp.StatusCode)
	}
	hash := sha256.New()
	n, err := io.Copy(io.MultiWriter(tmp, hash), io.LimitReader(resp.Body, limit+1))
	if err != nil {
		return nil, digest, err
	}
	if n > limit {
		return nil, digest, fmt.Errorf("response exceeds %d-byte limit", limit)
	}
	copy(digest[:], hash.Sum(nil))
	if _, err := tmp.Seek(0, io.SeekStart); err != nil {
		return nil, digest, err
	}
	keep = true
	return tmp, digest, nil
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

func verifyChecksum(digest [sha256.Size]byte, sums, assetName string) error {
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

func extractBinary(archive io.Reader, destination io.Writer, limit int64) error {
	gz, err := gzip.NewReader(archive)
	if err != nil {
		return fmt.Errorf("open archive: %w", err)
	}
	defer func() { _ = gz.Close() }()
	tr := tar.NewReader(gz)
	for {
		hdr, err := tr.Next()
		if err == io.EOF {
			return fmt.Errorf("archive contains no argmax binary")
		}
		if err != nil {
			return fmt.Errorf("read archive: %w", err)
		}
		if filepath.Base(hdr.Name) == "argmax" && hdr.Typeflag == tar.TypeReg {
			if hdr.Size <= 0 {
				return fmt.Errorf("archive contains an empty argmax binary")
			}
			if hdr.Size > limit {
				return fmt.Errorf("argmax binary exceeds %d-byte limit", limit)
			}
			n, err := io.Copy(destination, io.LimitReader(tr, limit+1))
			if err != nil {
				return fmt.Errorf("extract argmax binary: %w", err)
			}
			if n > limit {
				return fmt.Errorf("argmax binary exceeds %d-byte limit", limit)
			}
			return nil
		}
	}
}

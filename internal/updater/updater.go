// Package updater implements release checking and verified self-update
// (PRD 9.16, 9.17): channel-aware release selection, semantic version
// comparison, checksum-verified downloads, and atomic binary replacement.
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
	"io"
	"net/http"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"time"
)

// ReleaseBaseURL is the canonical release endpoint: the GitHub releases API
// root of the argmax repository. The ARGMAX_UPDATE_URL environment variable
// overrides it for tests and for mirrors serving the same JSON shape.
const ReleaseBaseURL = "https://api.github.com/repos/argmax-sh/argmax"

const (
	// networkTimeout bounds every release API and download request (UPD-003).
	networkTimeout = 5 * time.Second
	// maxMetadataSize bounds release metadata and checksums responses.
	maxMetadataSize = 32 << 20
	// maxArchiveSize bounds the downloaded release archive.
	maxArchiveSize = 256 << 20
	// maxBinarySize bounds the extracted binary payload.
	maxBinarySize = 512 << 20

	binaryName    = "argmax"
	checksumsName = "checksums.txt"
)

// httpClient is argument-free net/http with the UPD-003 timeout.
var httpClient = &http.Client{Timeout: networkTimeout}

// Info describes the newest release acceptable to a channel.
type Info struct {
	Version string // latest version tag without leading "v" for the channel
	URL     string // release page/download base
}

// asset mirrors one entry of the GitHub releases API assets array (and
// compatible mirrors).
type asset struct {
	Name string `json:"name"`
	URL  string `json:"browser_download_url"`
}

// release mirrors the GitHub releases API object shape.
type release struct {
	TagName    string  `json:"tag_name"`
	Prerelease bool    `json:"prerelease"`
	HTMLURL    string  `json:"html_url"`
	Assets     []asset `json:"assets"`
}

// Check queries the release API for the newest release acceptable to the
// channel (UPD-004): stable ignores prereleases entirely; nightly selects the
// newest prerelease, falling back to releases when there are none. Every
// request is bounded by a five-second network timeout (UPD-003). current is
// the running version; "dev" never reports an update (UPD-006).
func Check(ctx context.Context, channel, current string) (Info, bool, error) {
	if current == "" || current == "dev" {
		return Info{}, false, nil
	}
	rel, err := latestRelease(ctx, channel)
	if err != nil {
		return Info{}, false, err
	}
	info := Info{Version: strings.TrimPrefix(rel.TagName, "v"), URL: rel.url()}
	return info, Newer(current, info.Version), nil
}

// SelfUpdate downloads the correct OS/arch asset for the channel's newest
// release, verifies its SHA256 checksum against the release's checksums
// file, and atomically replaces the running binary (UPD-007/008). It reports
// current/latest versions and the already-current state on out. Any
// download, verification, permission, or replacement failure leaves the
// running binary intact and returns an error.
func SelfUpdate(ctx context.Context, channel, current string, out io.Writer) error {
	exe, err := os.Executable()
	if err != nil {
		return fmt.Errorf("locate running binary: %w", err)
	}
	if resolved, err := filepath.EvalSymlinks(exe); err == nil {
		exe = resolved
	}
	return selfUpdate(ctx, channel, current, exe, out)
}

// selfUpdate is the testable core of SelfUpdate: target is the binary path
// to replace instead of the resolved running executable.
func selfUpdate(ctx context.Context, channel, current, target string, out io.Writer) error {
	if current == "" || current == "dev" {
		return fmt.Errorf("self-update is not available for dev builds")
	}
	rel, err := latestRelease(ctx, channel)
	if err != nil {
		return err
	}
	latest := strings.TrimPrefix(rel.TagName, "v")
	_, _ = fmt.Fprintf(out, "current version: %s\n", current)
	_, _ = fmt.Fprintf(out, "latest version: %s\n", latest)
	if !Newer(current, latest) {
		_, _ = fmt.Fprintln(out, "argmax is already up to date")
		return nil
	}

	archive := findAsset(rel, assetNames(rel.TagName, latest)...)
	if archive == nil {
		return fmt.Errorf("release %s has no asset for %s/%s", rel.TagName, runtime.GOOS, runtime.GOARCH)
	}
	sums := findAsset(rel, checksumsName)
	if sums == nil {
		return fmt.Errorf("release %s has no %s asset", rel.TagName, checksumsName)
	}

	// Download and verify everything before touching the running binary
	// (UPD-008).
	archiveData, err := get(ctx, archive.URL, maxArchiveSize)
	if err != nil {
		return fmt.Errorf("download %s: %w", archive.Name, err)
	}
	sumsData, err := get(ctx, sums.URL, maxMetadataSize)
	if err != nil {
		return fmt.Errorf("download %s: %w", sums.Name, err)
	}
	if err := verifyChecksum(archiveData, sumsData, archive.Name); err != nil {
		return err
	}
	binary, err := extractBinary(archiveData)
	if err != nil {
		return fmt.Errorf("extract %s: %w", archive.Name, err)
	}
	if err := replaceBinary(target, binary); err != nil {
		return err
	}
	_, _ = fmt.Fprintf(out, "argmax updated to %s\n", latest)
	return nil
}

// apiBase resolves the release API root, honoring the mirror override.
func apiBase() string {
	if v := strings.TrimSpace(os.Getenv("ARGMAX_UPDATE_URL")); v != "" {
		return strings.TrimRight(v, "/")
	}
	return ReleaseBaseURL
}

// url reports the release page, falling back to the canonical tag URL when
// the API payload carries no html_url (e.g. a minimal mirror).
func (r *release) url() string {
	if r.HTMLURL != "" {
		return r.HTMLURL
	}
	return apiBase() + "/releases/tag/" + r.TagName
}

// latestRelease fetches the release list and selects the newest release
// acceptable to the channel (UPD-004).
func latestRelease(ctx context.Context, channel string) (*release, error) {
	switch channel {
	case "", "stable":
		channel = "stable"
	case "nightly":
	default:
		return nil, fmt.Errorf("unknown update channel %q: must be stable or nightly", channel)
	}
	releases, err := fetchReleases(ctx)
	if err != nil {
		return nil, err
	}

	var pool []*release
	if channel == "nightly" {
		for _, r := range releases {
			if r.Prerelease {
				pool = append(pool, r)
			}
		}
	}
	if len(pool) == 0 {
		// Stable always, nightly as a fallback when there are no
		// prereleases: consider full releases only.
		for _, r := range releases {
			if !r.Prerelease {
				pool = append(pool, r)
			}
		}
	}

	var best *release
	var bestV version
	for _, r := range pool {
		v, ok := parseVersion(r.TagName)
		if !ok {
			continue
		}
		if best == nil || compareVersions(v, bestV) > 0 {
			best, bestV = r, v
		}
	}
	if best == nil {
		return nil, fmt.Errorf("no releases found for the %s channel", channel)
	}
	return best, nil
}

// fetchReleases GETs and decodes the release list.
func fetchReleases(ctx context.Context) ([]*release, error) {
	data, err := get(ctx, apiBase()+"/releases", maxMetadataSize)
	if err != nil {
		return nil, fmt.Errorf("query releases: %w", err)
	}
	var releases []*release
	if err := json.Unmarshal(data, &releases); err != nil {
		return nil, fmt.Errorf("parse releases: %w", err)
	}
	return releases, nil
}

// get performs a bounded GET: the response body is capped at max bytes.
func get(ctx context.Context, url string, max int64) ([]byte, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return nil, err
	}
	req.Header.Set("Accept", "application/vnd.github+json")
	req.Header.Set("User-Agent", "argmax-updater")
	resp, err := httpClient.Do(req)
	if err != nil {
		return nil, err
	}
	defer func() { _ = resp.Body.Close() }()
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("%s: unexpected status %s", url, resp.Status)
	}
	data, err := io.ReadAll(io.LimitReader(resp.Body, max+1))
	if err != nil {
		return nil, err
	}
	if int64(len(data)) > max {
		return nil, fmt.Errorf("%s: response exceeds %d bytes", url, max)
	}
	return data, nil
}

// assetNames returns the accepted archive names for a release, most
// specific first. The goreleaser pipeline publishes unversioned
// "argmax_<os>_<arch>.tar.gz" assets (also used by scripts/install.sh);
// versioned variants are accepted for mirrors that keep them.
func assetNames(tag, version string) []string {
	names := []string{
		fmt.Sprintf("%s_%s_%s.tar.gz", binaryName, runtime.GOOS, runtime.GOARCH),
		fmt.Sprintf("%s_%s_%s_%s.tar.gz", binaryName, version, runtime.GOOS, runtime.GOARCH),
	}
	if tag != "" && tag != version {
		names = append(names, fmt.Sprintf("%s_%s_%s_%s.tar.gz", binaryName, tag, runtime.GOOS, runtime.GOARCH))
	}
	return names
}

// findAsset locates the first asset matching any of the given names.
func findAsset(rel *release, names ...string) *asset {
	for _, name := range names {
		for i := range rel.Assets {
			if rel.Assets[i].Name == name {
				return &rel.Assets[i]
			}
		}
	}
	return nil
}

// verifyChecksum checks the archive against its checksums.txt line
// (UPD-007).
func verifyChecksum(archive, sums []byte, name string) error {
	want, err := checksumFor(sums, name)
	if err != nil {
		return err
	}
	sum := sha256.Sum256(archive)
	got := hex.EncodeToString(sum[:])
	if !strings.EqualFold(got, want) {
		return fmt.Errorf("checksum mismatch for %s: expected %s, got %s", name, want, got)
	}
	return nil
}

// checksumFor parses the sha256sum-format checksums file for one asset's
// expected hex digest.
func checksumFor(sums []byte, name string) (string, error) {
	for _, line := range strings.Split(string(sums), "\n") {
		fields := strings.Fields(line)
		// sha256sum marks binary-mode entries with a "*" prefix.
		if len(fields) == 2 && strings.TrimPrefix(fields[1], "*") == name {
			return strings.ToLower(fields[0]), nil
		}
	}
	return "", fmt.Errorf("%s has no entry for %s", checksumsName, name)
}

// extractBinary pulls the argmax binary entry out of a .tar.gz release
// archive.
func extractBinary(archive []byte) ([]byte, error) {
	gz, err := gzip.NewReader(bytes.NewReader(archive))
	if err != nil {
		return nil, err
	}
	defer func() { _ = gz.Close() }()
	tr := tar.NewReader(gz)
	for {
		hdr, err := tr.Next()
		if err == io.EOF {
			return nil, fmt.Errorf("archive does not contain %q", binaryName)
		}
		if err != nil {
			return nil, err
		}
		if hdr.Typeflag != tar.TypeReg || filepath.Base(hdr.Name) != binaryName {
			continue
		}
		if hdr.Size > maxBinarySize {
			return nil, fmt.Errorf("%s entry exceeds %d bytes", binaryName, maxBinarySize)
		}
		return io.ReadAll(io.LimitReader(tr, maxBinarySize+1))
	}
}

// replaceBinary atomically swaps the binary at target: write to a temp file
// in the same directory, chmod 0755, then rename (UPD-007). Any failure
// removes the temp file and leaves the original binary intact (UPD-008).
func replaceBinary(target string, data []byte) error {
	dir := filepath.Dir(target)
	tmp, err := os.CreateTemp(dir, ".argmax-update-*")
	if err != nil {
		return fmt.Errorf("create temp file in %s: %w", dir, err)
	}
	tmpName := tmp.Name()
	defer func() { _ = os.Remove(tmpName) }()
	if _, err := tmp.Write(data); err != nil {
		_ = tmp.Close()
		return fmt.Errorf("write %s: %w", tmpName, err)
	}
	if err := tmp.Chmod(0o755); err != nil {
		_ = tmp.Close()
		return fmt.Errorf("chmod %s: %w", tmpName, err)
	}
	if err := tmp.Sync(); err != nil {
		_ = tmp.Close()
		return fmt.Errorf("sync %s: %w", tmpName, err)
	}
	if err := tmp.Close(); err != nil {
		return fmt.Errorf("close %s: %w", tmpName, err)
	}
	if err := os.Rename(tmpName, target); err != nil {
		return fmt.Errorf("replace %s: %w", target, err)
	}
	return nil
}

// version is a parsed semantic version: numeric major/minor/patch plus an
// optional prerelease string ("" is a full release).
type version struct {
	major, minor, patch int
	pre                 string
}

// parseVersion tolerates a leading "v", splits the prerelease on "-", and
// ignores "+" build metadata. Missing minor/patch components read as zero.
func parseVersion(s string) (version, bool) {
	s = strings.TrimSpace(s)
	s = strings.TrimPrefix(s, "v")
	if i := strings.IndexByte(s, '+'); i >= 0 {
		s = s[:i]
	}
	var v version
	if i := strings.IndexByte(s, '-'); i >= 0 {
		v.pre = s[i+1:]
		s = s[:i]
	}
	parts := strings.Split(s, ".")
	if len(parts) > 3 {
		return version{}, false
	}
	nums := []*int{&v.major, &v.minor, &v.patch}
	for i, p := range parts {
		n, err := strconv.Atoi(p)
		if err != nil || n < 0 {
			return version{}, false
		}
		*nums[i] = n
	}
	return v, true
}

// Newer reports whether latest is newer than current under semantic
// major/minor/patch comparison with correct prerelease policy (UPD-004):
// 1.2.3 < 1.2.4 and 1.2.3-nightly.1 < 1.2.3. Channel filtering happens in
// Check. An unparseable version never counts as newer.
func Newer(current, latest string) bool {
	cur, ok := parseVersion(current)
	if !ok {
		return false
	}
	lat, ok := parseVersion(latest)
	if !ok {
		return false
	}
	return compareVersions(lat, cur) > 0
}

// compareVersions orders two parsed versions: numeric major/minor/patch,
// then prerelease policy — a full release outranks the same version with a
// prerelease.
func compareVersions(a, b version) int {
	if a.major != b.major {
		return cmpInt(a.major, b.major)
	}
	if a.minor != b.minor {
		return cmpInt(a.minor, b.minor)
	}
	if a.patch != b.patch {
		return cmpInt(a.patch, b.patch)
	}
	switch {
	case a.pre == b.pre:
		return 0
	case a.pre == "":
		return 1
	case b.pre == "":
		return -1
	}
	return comparePrerelease(a.pre, b.pre)
}

// comparePrerelease orders dot-separated prerelease identifiers per semver:
// numeric identifiers compare numerically and sort before alphanumeric ones;
// a longer list wins when the shared prefix is equal.
func comparePrerelease(a, b string) int {
	as, bs := strings.Split(a, "."), strings.Split(b, ".")
	for i := 0; i < len(as) && i < len(bs); i++ {
		if as[i] == bs[i] {
			continue
		}
		an, aerr := strconv.Atoi(as[i])
		bn, berr := strconv.Atoi(bs[i])
		switch {
		case aerr == nil && berr == nil:
			if an != bn {
				return cmpInt(an, bn)
			}
		case aerr == nil:
			return -1
		case berr == nil:
			return 1
		default:
			if c := strings.Compare(as[i], bs[i]); c != 0 {
				return c
			}
		}
	}
	return cmpInt(len(as), len(bs))
}

func cmpInt(a, b int) int {
	switch {
	case a < b:
		return -1
	case a > b:
		return 1
	}
	return 0
}

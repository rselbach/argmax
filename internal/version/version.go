// Package version provides strict semantic-version parsing and automatic-update policy.
package version

import (
	"cmp"
	"fmt"
	"io"
	"strconv"
	"strings"
	"unicode"
)

const (
	// MaxVersionBytes is the maximum accepted byte length of a version or release tag.
	MaxVersionBytes = 256

	maxPrereleaseIdentifiers = 64
)

// runningVersion may be replaced in release builds with:
//
//	-ldflags=-X=github.com/rselbach/argmax/internal/version.runningVersion=<version>
var runningVersion = "0.1.0"

// RunningVersion returns the validated semantic version embedded in this executable.
func RunningVersion() (Version, error) {
	return Parse(runningVersion)
}

// Version is a validated semantic version.
type Version struct {
	text       string
	major      string
	minor      string
	patch      string
	prerelease []prereleaseIdentifier
}

type prereleaseIdentifier struct {
	text    string
	numeric bool
}

// Parse parses a strict semantic version without a tag prefix.
//
// Build metadata is validated and retained by String, but does not affect
// precedence.
func Parse(value string) (Version, error) {
	return parse(value, false)
}

// ParseReleaseTag parses a strict semantic version with an optional single
// lowercase "v" prefix.
func ParseReleaseTag(value string) (Version, error) {
	return parse(value, true)
}

func parse(value string, allowTagPrefix bool) (Version, error) {
	if value == "" {
		return Version{}, newError(ErrorEmpty)
	}
	if len(value) > MaxVersionBytes {
		return Version{}, newBoundedError(ErrorTooLong, len(value), MaxVersionBytes)
	}
	if strings.TrimFunc(value, unicode.IsSpace) != value {
		return Version{}, newError(ErrorInvalidSyntax)
	}

	semantic := value
	if allowTagPrefix {
		semantic = strings.TrimPrefix(semantic, "v")
	}
	if semantic == "" || strings.HasPrefix(semantic, "v") {
		return Version{}, newError(ErrorInvalidSyntax)
	}

	withoutBuild, build, err := splitOnceUnique(semantic, '+')
	if err != nil {
		return Version{}, err
	}
	if build != "" {
		if err := validateDotIdentifiers(build, false); err != nil {
			return Version{}, err
		}
	}

	core := withoutBuild
	prereleaseText := ""
	if before, after, found := strings.Cut(withoutBuild, "-"); found {
		if before == "" || after == "" {
			return Version{}, newError(ErrorInvalidSyntax)
		}
		core = before
		prereleaseText = after
	}

	coreParts := strings.Split(core, ".")
	if len(coreParts) != 3 {
		return Version{}, newError(ErrorInvalidCore)
	}
	major, err := parseCoreNumber(coreParts[0])
	if err != nil {
		return Version{}, err
	}
	minor, err := parseCoreNumber(coreParts[1])
	if err != nil {
		return Version{}, err
	}
	patch, err := parseCoreNumber(coreParts[2])
	if err != nil {
		return Version{}, err
	}

	var prerelease []prereleaseIdentifier
	if prereleaseText != "" {
		prerelease, err = parsePrerelease(prereleaseText)
		if err != nil {
			return Version{}, err
		}
	}

	return Version{
		text:       semantic,
		major:      major,
		minor:      minor,
		patch:      patch,
		prerelease: prerelease,
	}, nil
}

// String returns the validated semantic-version text without a release-tag prefix.
func (v Version) String() string {
	return v.text
}

// GoString returns a structural representation that redacts version text and metadata.
func (v Version) GoString() string {
	return v.debugString()
}

// Format preserves String for ordinary formatting and uses the redacted
// structural representation for detailed Go formatting.
func (v Version) Format(state fmt.State, verb rune) {
	if verb == 'v' && (state.Flag('+') || state.Flag('#')) {
		_, _ = io.WriteString(state, v.debugString())
		return
	}

	switch verb {
	case 's', 'v':
		_, _ = io.WriteString(state, v.text)
	case 'q':
		_, _ = io.WriteString(state, strconv.Quote(v.text))
	default:
		_, _ = fmt.Fprintf(state, "%%!%c(version.Version)", verb)
	}
}

func (v Version) debugString() string {
	return fmt.Sprintf(
		"SemanticVersion { bytes: %d, major_digits: %d, minor_digits: %d, patch_digits: %d, prerelease_identifiers: %d }",
		len(v.text),
		len(v.major),
		len(v.minor),
		len(v.patch),
		len(v.prerelease),
	)
}

// Major returns the major version component.
func (v Version) Major() string {
	return v.major
}

// Minor returns the minor version component.
func (v Version) Minor() string {
	return v.minor
}

// Patch returns the patch version component.
func (v Version) Patch() string {
	return v.patch
}

// IsPrerelease reports whether v contains prerelease identifiers.
func (v Version) IsPrerelease() bool {
	return len(v.prerelease) != 0
}

// Compare compares semantic-version precedence, ignoring build metadata.
func (v Version) Compare(other Version) int {
	if order := compareNumericText(v.major, other.major); order != 0 {
		return order
	}
	if order := compareNumericText(v.minor, other.minor); order != 0 {
		return order
	}
	if order := compareNumericText(v.patch, other.patch); order != 0 {
		return order
	}
	return comparePrerelease(v.prerelease, other.prerelease)
}

// SameIdentity reports whether both versions have identical text, including
// prerelease and build metadata.
func (v Version) SameIdentity(other Version) bool {
	return v.text == other.text
}

// SamePrecedence reports whether both versions have equal precedence after
// ignoring build metadata.
func (v Version) SamePrecedence(other Version) bool {
	return v.Compare(other) == 0
}

// ReleaseKind is the trusted release-feed classification of a remote artifact.
type ReleaseKind uint8

const (
	// StableRelease identifies a final release artifact.
	StableRelease ReleaseKind = iota
	// NightlyRelease identifies a nightly artifact.
	NightlyRelease
)

// String returns the release classification name.
func (k ReleaseKind) String() string {
	switch k {
	case StableRelease:
		return "Stable"
	case NightlyRelease:
		return "Nightly"
	default:
		return fmt.Sprintf("ReleaseKind(%d)", k)
	}
}

// Channel is the configured automatic-update channel.
type Channel uint8

const (
	// StableChannel accepts only stable release artifacts.
	StableChannel Channel = iota
	// NightlyChannel accepts stable promotions and nightly prereleases.
	NightlyChannel
)

// RemoteRelease associates an unparsed remote tag with its trusted artifact
// classification. Its formatted representation never includes the tag text.
type RemoteRelease struct {
	tag  string
	kind ReleaseKind
}

// NewRemoteRelease associates a remote tag with its trusted artifact classification.
func NewRemoteRelease(tag string, kind ReleaseKind) RemoteRelease {
	return RemoteRelease{tag: tag, kind: kind}
}

// Tag returns the unparsed remote release tag.
func (r RemoteRelease) Tag() string {
	return r.tag
}

// Kind returns the trusted artifact classification.
func (r RemoteRelease) Kind() ReleaseKind {
	return r.kind
}

// String returns a structural representation that redacts the untrusted tag.
func (r RemoteRelease) String() string {
	return r.debugString()
}

// GoString returns a structural representation that redacts the untrusted tag.
func (r RemoteRelease) GoString() string {
	return r.debugString()
}

func (r RemoteRelease) debugString() string {
	return fmt.Sprintf("RemoteRelease { kind: %s, tag_bytes: %d }", r.kind, len(r.tag))
}

// DecisionKind classifies an automatic-update decision.
type DecisionKind uint8

const (
	// DecisionAvailable means an eligible remote version is newer.
	DecisionAvailable DecisionKind = iota + 1
	// DecisionCurrent means the remote version is eligible but not newer.
	DecisionCurrent
	// DecisionChannelMismatch means the stable channel excluded a nightly artifact.
	DecisionChannelMismatch
	// DecisionInvalidRemoteMetadata means trusted metadata contradicted the version form.
	DecisionInvalidRemoteMetadata
	// DecisionDevelopmentBuild means the running version is empty or "dev".
	DecisionDevelopmentBuild
	// DecisionInvalidCurrentVersion means the running version is malformed.
	DecisionInvalidCurrentVersion
	// DecisionInvalidRemoteVersion means the remote release tag is malformed.
	DecisionInvalidRemoteVersion
)

// String returns the decision classification name.
func (k DecisionKind) String() string {
	switch k {
	case DecisionAvailable:
		return "Available"
	case DecisionCurrent:
		return "Current"
	case DecisionChannelMismatch:
		return "ChannelMismatch"
	case DecisionInvalidRemoteMetadata:
		return "InvalidRemoteMetadata"
	case DecisionDevelopmentBuild:
		return "DevelopmentBuild"
	case DecisionInvalidCurrentVersion:
		return "InvalidCurrentVersion"
	case DecisionInvalidRemoteVersion:
		return "InvalidRemoteVersion"
	default:
		return fmt.Sprintf("DecisionKind(%d)", k)
	}
}

// Decision is the safe result of comparing a running build with one remote release.
type Decision struct {
	kind    DecisionKind
	version Version
	err     error
}

// Kind returns the decision classification.
func (d Decision) Kind() DecisionKind {
	return d.kind
}

// Version returns the available remote version. The boolean is false for all
// decisions other than DecisionAvailable.
func (d Decision) Version() (Version, bool) {
	return d.version, d.kind == DecisionAvailable
}

// Err returns the parsing error for invalid current or remote versions.
func (d Decision) Err() error {
	return d.err
}

// String returns a redacted structural representation of the decision.
func (d Decision) String() string {
	switch d.kind {
	case DecisionAvailable:
		return fmt.Sprintf("Available(%s)", d.version.debugString())
	case DecisionInvalidCurrentVersion, DecisionInvalidRemoteVersion:
		if parseErr, ok := d.err.(*Error); ok {
			return fmt.Sprintf("%s(%s)", d.kind, parseErr.debugString())
		}
		return fmt.Sprintf("%s(%v)", d.kind, d.err)
	default:
		return d.kind.String()
	}
}

// GoString returns a redacted structural representation of the decision.
func (d Decision) GoString() string {
	return d.String()
}

// DecideAutomaticUpdate applies automatic-update version and channel policy to
// one remote release.
func DecideAutomaticUpdate(current string, remote RemoteRelease, channel Channel) Decision {
	if current == "" || equalASCIIFold(current, "dev") {
		return Decision{kind: DecisionDevelopmentBuild}
	}

	currentVersion, err := Parse(current)
	if err != nil {
		return Decision{kind: DecisionInvalidCurrentVersion, err: err}
	}
	remoteVersion, err := ParseReleaseTag(remote.Tag())
	if err != nil {
		return Decision{kind: DecisionInvalidRemoteVersion, err: err}
	}

	metadataIsConsistent := false
	switch remote.Kind() {
	case StableRelease:
		metadataIsConsistent = !remoteVersion.IsPrerelease()
	case NightlyRelease:
		metadataIsConsistent = remoteVersion.IsPrerelease()
	}
	if !metadataIsConsistent {
		return Decision{kind: DecisionInvalidRemoteMetadata}
	}
	if channel != NightlyChannel && remote.Kind() == NightlyRelease {
		return Decision{kind: DecisionChannelMismatch}
	}
	if remoteVersion.Compare(currentVersion) > 0 {
		return Decision{kind: DecisionAvailable, version: remoteVersion}
	}
	return Decision{kind: DecisionCurrent}
}

// ErrorKind identifies why version text was rejected.
type ErrorKind uint8

const (
	// ErrorEmpty means no version text was supplied.
	ErrorEmpty ErrorKind = iota
	// ErrorTooLong means the version exceeded MaxVersionBytes.
	ErrorTooLong
	// ErrorInvalidSyntax means separators, whitespace, or a tag prefix were malformed.
	ErrorInvalidSyntax
	// ErrorInvalidCore means the core was not exactly three decimal components.
	ErrorInvalidCore
	// ErrorLeadingZero means a numeric identifier used a forbidden leading zero.
	ErrorLeadingZero
	// ErrorInvalidIdentifier means a prerelease or build identifier was invalid.
	ErrorInvalidIdentifier
	// ErrorTooManyPrereleaseIdentifiers means the prerelease identifier bound was exceeded.
	ErrorTooManyPrereleaseIdentifiers
)

// Error describes a rejected version string without retaining that string.
type Error struct {
	kind  ErrorKind
	bytes int
	limit int
}

func newError(kind ErrorKind) *Error {
	return &Error{kind: kind}
}

func newBoundedError(kind ErrorKind, bytes, limit int) *Error {
	return &Error{kind: kind, bytes: bytes, limit: limit}
}

// Kind returns the rejection classification.
func (e *Error) Kind() ErrorKind {
	return e.kind
}

// Bytes returns the observed byte length for ErrorTooLong and zero otherwise.
func (e *Error) Bytes() int {
	return e.bytes
}

// Limit returns the applicable defensive limit for bounded errors and zero otherwise.
func (e *Error) Limit() int {
	return e.limit
}

// Error returns a safe description that does not include rejected version text.
func (e *Error) Error() string {
	switch e.kind {
	case ErrorEmpty:
		return "version is empty"
	case ErrorTooLong:
		return fmt.Sprintf("version is %d bytes; limit is %d", e.bytes, e.limit)
	case ErrorInvalidSyntax:
		return "version syntax is invalid"
	case ErrorInvalidCore:
		return "version must contain decimal major, minor, and patch"
	case ErrorLeadingZero:
		return "numeric version identifier has a leading zero"
	case ErrorInvalidIdentifier:
		return "prerelease or build identifier is invalid"
	case ErrorTooManyPrereleaseIdentifiers:
		return fmt.Sprintf("prerelease has more than %d identifiers", e.limit)
	default:
		return fmt.Sprintf("unknown version error (%d)", e.kind)
	}
}

// GoString returns the Rust-compatible structural error representation.
func (e *Error) GoString() string {
	return e.debugString()
}

func (e *Error) debugString() string {
	switch e.kind {
	case ErrorEmpty:
		return "Empty"
	case ErrorTooLong:
		return fmt.Sprintf("TooLong { bytes: %d, limit: %d }", e.bytes, e.limit)
	case ErrorInvalidSyntax:
		return "InvalidSyntax"
	case ErrorInvalidCore:
		return "InvalidCore"
	case ErrorLeadingZero:
		return "LeadingZero"
	case ErrorInvalidIdentifier:
		return "InvalidIdentifier"
	case ErrorTooManyPrereleaseIdentifiers:
		return fmt.Sprintf("TooManyPrereleaseIdentifiers { limit: %d }", e.limit)
	default:
		return fmt.Sprintf("ErrorKind(%d)", e.kind)
	}
}

func splitOnceUnique(value string, separator byte) (string, string, error) {
	index := strings.IndexByte(value, separator)
	if index == -1 {
		return value, "", nil
	}
	left, right := value[:index], value[index+1:]
	if left == "" || right == "" || strings.IndexByte(right, separator) != -1 {
		return "", "", newError(ErrorInvalidSyntax)
	}
	return left, right, nil
}

func parseCoreNumber(value string) (string, error) {
	if value == "" || !isASCIIDigits(value) {
		return "", newError(ErrorInvalidCore)
	}
	if len(value) > 1 && value[0] == '0' {
		return "", newError(ErrorLeadingZero)
	}
	return value, nil
}

func parsePrerelease(value string) ([]prereleaseIdentifier, error) {
	if err := validateDotIdentifiers(value, true); err != nil {
		return nil, err
	}

	parts := strings.Split(value, ".")
	identifiers := make([]prereleaseIdentifier, 0, len(parts))
	for _, identifier := range parts {
		identifiers = append(identifiers, prereleaseIdentifier{
			text:    identifier,
			numeric: isASCIIDigits(identifier),
		})
	}
	return identifiers, nil
}

func validateDotIdentifiers(value string, prerelease bool) error {
	parts := strings.Split(value, ".")
	for _, identifier := range parts {
		if identifier == "" || !isASCIIIdentifier(identifier) {
			return newError(ErrorInvalidIdentifier)
		}
		if prerelease && len(identifier) > 1 && isASCIIDigits(identifier) && identifier[0] == '0' {
			return newError(ErrorLeadingZero)
		}
	}
	if prerelease && len(parts) > maxPrereleaseIdentifiers {
		return newBoundedError(
			ErrorTooManyPrereleaseIdentifiers,
			0,
			maxPrereleaseIdentifiers,
		)
	}
	return nil
}

func equalASCIIFold(left, right string) bool {
	if len(left) != len(right) {
		return false
	}
	for i := range len(left) {
		leftByte := left[i]
		rightByte := right[i]
		if leftByte >= 'A' && leftByte <= 'Z' {
			leftByte += 'a' - 'A'
		}
		if rightByte >= 'A' && rightByte <= 'Z' {
			rightByte += 'a' - 'A'
		}
		if leftByte != rightByte {
			return false
		}
	}
	return true
}

func isASCIIDigits(value string) bool {
	for i := range len(value) {
		if value[i] < '0' || value[i] > '9' {
			return false
		}
	}
	return true
}

func isASCIIIdentifier(value string) bool {
	for i := range len(value) {
		char := value[i]
		if (char < '0' || char > '9') &&
			(char < 'A' || char > 'Z') &&
			(char < 'a' || char > 'z') &&
			char != '-' {
			return false
		}
	}
	return true
}

func comparePrerelease(left, right []prereleaseIdentifier) int {
	switch {
	case len(left) == 0 && len(right) == 0:
		return 0
	case len(left) == 0:
		return 1
	case len(right) == 0:
		return -1
	}

	for i := 0; i < min(len(left), len(right)); i++ {
		if order := compareIdentifier(left[i], right[i]); order != 0 {
			return order
		}
	}
	return cmp.Compare(len(left), len(right))
}

func compareIdentifier(left, right prereleaseIdentifier) int {
	switch {
	case left.numeric && right.numeric:
		return compareNumericText(left.text, right.text)
	case left.numeric:
		return -1
	case right.numeric:
		return 1
	default:
		return cmp.Compare(left.text, right.text)
	}
}

func compareNumericText(left, right string) int {
	if order := cmp.Compare(len(left), len(right)); order != 0 {
		return order
	}
	return cmp.Compare(left, right)
}

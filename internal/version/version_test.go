package version

import (
	"errors"
	"fmt"
	"strings"
	"testing"
)

func mustParse(t *testing.T, value string) Version {
	t.Helper()
	parsed, err := Parse(value)
	if err != nil {
		t.Fatalf("Parse(%q): %v", value, err)
	}
	return parsed
}

func release(tag string, kind ReleaseKind) RemoteRelease {
	return NewRemoteRelease(tag, kind)
}

func TestRunningVersion(t *testing.T) {
	got, err := RunningVersion()
	if err != nil {
		t.Fatalf("RunningVersion(): %v", err)
	}
	if got.String() != runningVersion {
		t.Fatalf("RunningVersion().String() = %q, want %q", got.String(), runningVersion)
	}
}

func TestParseComponents(t *testing.T) {
	got, err := Parse("12.34.56-rc.2+macos.arm64")
	if err != nil {
		t.Fatalf("Parse(): %v", err)
	}
	if got.Major() != "12" {
		t.Errorf("Major() = %q, want 12", got.Major())
	}
	if got.Minor() != "34" {
		t.Errorf("Minor() = %q, want 34", got.Minor())
	}
	if got.Patch() != "56" {
		t.Errorf("Patch() = %q, want 56", got.Patch())
	}
	if !got.IsPrerelease() {
		t.Error("IsPrerelease() = false, want true")
	}
	if got.String() != "12.34.56-rc.2+macos.arm64" {
		t.Errorf("String() = %q, want original semantic version", got.String())
	}
}

func TestParseReleaseTagPrefix(t *testing.T) {
	tests := map[string]struct {
		value string
		want  string
		err   ErrorKind
	}{
		"no prefix":        {value: "1.2.3", want: "1.2.3"},
		"lowercase prefix": {value: "v1.2.3", want: "1.2.3"},
		"uppercase prefix": {value: "V1.2.3", err: ErrorInvalidCore},
		"double prefix":    {value: "vv1.2.3", err: ErrorInvalidSyntax},
		"prefix only":      {value: "v", err: ErrorInvalidSyntax},
	}

	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			got, err := ParseReleaseTag(tc.value)
			if tc.err != 0 {
				assertErrorKind(t, err, tc.err)
				return
			}
			if err != nil {
				t.Fatalf("ParseReleaseTag(%q): %v", tc.value, err)
			}
			if got.String() != tc.want {
				t.Errorf("String() = %q, want %q", got.String(), tc.want)
			}
		})
	}

	_, err := Parse("v1.2.3")
	assertErrorKind(t, err, ErrorInvalidSyntax)
}

func TestVersionFormattingRedactsDetailedOutput(t *testing.T) {
	got := mustParse(t, "1.2.3-secret-api-token+private-build")
	wantDebug := "SemanticVersion { bytes: 36, major_digits: 1, minor_digits: 1, patch_digits: 1, prerelease_identifiers: 1 }"

	formats := map[string]struct {
		format string
		want   string
	}{
		"display":         {format: "%v", want: got.String()},
		"string":          {format: "%s", want: got.String()},
		"quoted":          {format: "%q", want: fmt.Sprintf("%q", got.String())},
		"detailed":        {format: "%+v", want: wantDebug},
		"Go syntax":       {format: "%#v", want: wantDebug},
		"direct GoString": {format: "%s", want: wantDebug},
	}

	for name, tc := range formats {
		t.Run(name, func(t *testing.T) {
			var formatted string
			if name == "direct GoString" {
				formatted = got.GoString()
			} else {
				formatted = fmt.Sprintf(tc.format, got)
			}
			if formatted != tc.want {
				t.Errorf("formatted = %q, want %q", formatted, tc.want)
			}
			if name != "display" && name != "string" && name != "quoted" {
				if strings.Contains(formatted, "secret-api-token") || strings.Contains(formatted, "private-build") {
					t.Errorf("detailed output exposed version text: %s", formatted)
				}
			}
		})
	}
}

func TestRemoteReleaseFormattingRedactsTag(t *testing.T) {
	tag := "\x1b[31msecret-api-token"
	got := release(tag, NightlyRelease)
	want := fmt.Sprintf("RemoteRelease { kind: Nightly, tag_bytes: %d }", len(tag))

	for _, format := range []string{"%v", "%+v", "%#v", "%s"} {
		formatted := fmt.Sprintf(format, got)
		if formatted != want {
			t.Errorf("Sprintf(%q) = %q, want %q", format, formatted, want)
		}
		if strings.Contains(formatted, "secret-api-token") {
			t.Errorf("Sprintf(%q) exposed tag: %s", format, formatted)
		}
	}
	if got.Tag() != tag {
		t.Errorf("Tag() = %q, want original tag", got.Tag())
	}
	if got.Kind() != NightlyRelease {
		t.Errorf("Kind() = %v, want NightlyRelease", got.Kind())
	}
}

func TestInvalidRemoteDecisionDoesNotRetainTag(t *testing.T) {
	got := DecideAutomaticUpdate(
		"1.0.0",
		release("secret-api-token", StableRelease),
		StableChannel,
	)
	if got.Kind() != DecisionInvalidRemoteVersion {
		t.Fatalf("Kind() = %v, want InvalidRemoteVersion", got.Kind())
	}
	for _, formatted := range []string{got.String(), got.GoString(), fmt.Sprintf("%#v", got)} {
		if strings.Contains(formatted, "secret-api-token") {
			t.Errorf("decision output exposed tag: %s", formatted)
		}
	}
}

func TestPrereleasePrecedenceStandardSequence(t *testing.T) {
	ordered := []string{
		"1.0.0-alpha",
		"1.0.0-alpha.1",
		"1.0.0-alpha.beta",
		"1.0.0-beta",
		"1.0.0-beta.2",
		"1.0.0-beta.11",
		"1.0.0-rc.1",
		"1.0.0",
	}

	for i := 0; i < len(ordered)-1; i++ {
		left := mustParse(t, ordered[i])
		right := mustParse(t, ordered[i+1])
		if left.Compare(right) >= 0 {
			t.Errorf("Compare(%q, %q) = %d, want < 0", left, right, left.Compare(right))
		}
	}
}

func TestPrecedenceOrderingProperties(t *testing.T) {
	texts := []string{
		"0.0.0",
		"1.0.0-alpha",
		"1.0.0-alpha.999999999999999999999999999999",
		"1.0.0-alpha.beta",
		"1.0.0-beta",
		"1.0.0",
		"999999999999999999999999999999.0.0",
	}
	versions := make([]Version, len(texts))
	for i, text := range texts {
		versions[i] = mustParse(t, text)
	}

	for leftIndex, left := range versions {
		for rightIndex, right := range versions {
			got := left.Compare(right)
			if got != -right.Compare(left) {
				t.Errorf("comparison is not antisymmetric for %q and %q", left, right)
			}
			want := 0
			if leftIndex < rightIndex {
				want = -1
			} else if leftIndex > rightIndex {
				want = 1
			}
			if got != want {
				t.Errorf("Compare(%q, %q) = %d, want %d", left, right, got, want)
			}
		}
	}

	for first := range versions {
		for second := first; second < len(versions); second++ {
			for third := second; third < len(versions); third++ {
				if versions[first].Compare(versions[second]) > 0 ||
					versions[second].Compare(versions[third]) > 0 ||
					versions[first].Compare(versions[third]) > 0 {
					t.Errorf("comparison is not transitive for indexes %d, %d, %d", first, second, third)
				}
			}
		}
	}
}

func TestVersionComparison(t *testing.T) {
	tests := map[string]struct {
		left         string
		right        string
		wantCompare  int
		wantIdentity bool
	}{
		"numeric core": {
			left: "9.100.7", right: "10.0.0", wantCompare: -1,
		},
		"large numeric core": {
			left: "18446744073709551616.0.0", right: "18446744073709551615.999.999", wantCompare: 1,
		},
		"build metadata ignored": {
			left: "1.2.3+first", right: "1.2.3+second", wantIdentity: false,
		},
		"identical": {
			left: "1.2.3-rc.1+build", right: "1.2.3-rc.1+build", wantIdentity: true,
		},
		"numeric before alphanumeric": {
			left: "1.0.0-1", right: "1.0.0-alpha", wantCompare: -1,
		},
		"shorter prerelease first": {
			left: "1.0.0-alpha", right: "1.0.0-alpha.0", wantCompare: -1,
		},
	}

	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			left := mustParse(t, tc.left)
			right := mustParse(t, tc.right)
			if got := left.Compare(right); got != tc.wantCompare {
				t.Errorf("Compare() = %d, want %d", got, tc.wantCompare)
			}
			if got := left.SamePrecedence(right); got != (tc.wantCompare == 0) {
				t.Errorf("SamePrecedence() = %t, want %t", got, tc.wantCompare == 0)
			}
			if got := left.SameIdentity(right); got != tc.wantIdentity {
				t.Errorf("SameIdentity() = %t, want %t", got, tc.wantIdentity)
			}
		})
	}
}

func TestIdentifiersMayContainHyphens(t *testing.T) {
	got := mustParse(t, "1.0.0-nightly-20260729+build-arm64")
	if !got.IsPrerelease() {
		t.Error("IsPrerelease() = false, want true")
	}
	if got.String() != "1.0.0-nightly-20260729+build-arm64" {
		t.Errorf("String() = %q, want original", got.String())
	}
}

func TestParseRejectsMalformedVersions(t *testing.T) {
	tests := map[string]ErrorKind{
		"":                 ErrorEmpty,
		"1":                ErrorInvalidCore,
		"1.2":              ErrorInvalidCore,
		"1.2.3.4":          ErrorInvalidCore,
		"01.2.3":           ErrorLeadingZero,
		"1.02.3":           ErrorLeadingZero,
		"1.2.03":           ErrorLeadingZero,
		"1.2.-3":           ErrorInvalidCore,
		"1.2.3-":           ErrorInvalidSyntax,
		"1.2.3-alpha..one": ErrorInvalidIdentifier,
		"1.2.3-alpha_1":    ErrorInvalidIdentifier,
		"1.2.3-01":         ErrorLeadingZero,
		"1.2.3+":           ErrorInvalidSyntax,
		"1.2.3+meta..one":  ErrorInvalidIdentifier,
		"1.2.3+meta+again": ErrorInvalidSyntax,
		" 1.2.3":           ErrorInvalidSyntax,
		"1.2.3 ":           ErrorInvalidSyntax,
		"1.2.3\n":          ErrorInvalidSyntax,
		"1.2.3+méta":       ErrorInvalidIdentifier,
		"1.2.3-alpha.β":    ErrorInvalidIdentifier,
	}

	for value, want := range tests {
		t.Run(fmt.Sprintf("%q", value), func(t *testing.T) {
			_, err := Parse(value)
			assertErrorKind(t, err, want)
		})
	}
}

func TestPrereleaseIdentifierBound(t *testing.T) {
	maximum := "1.0.0-" + strings.Repeat("a.", maxPrereleaseIdentifiers-1) + "a+build.0001"
	if _, err := Parse(maximum); err != nil {
		t.Fatalf("Parse(maximum identifiers): %v", err)
	}

	excessive := "1.0.0-" + strings.Repeat("a.", maxPrereleaseIdentifiers) + "a"
	_, err := Parse(excessive)
	assertErrorKind(t, err, ErrorTooManyPrereleaseIdentifiers)
	var parseErr *Error
	if !errors.As(err, &parseErr) {
		t.Fatalf("error type = %T, want *Error", err)
	}
	if parseErr.Limit() != maxPrereleaseIdentifiers {
		t.Errorf("Limit() = %d, want %d", parseErr.Limit(), maxPrereleaseIdentifiers)
	}

	invalidLast := "1.0.0-" + strings.Repeat("a.", maxPrereleaseIdentifiers) + "_"
	_, err = Parse(invalidLast)
	assertErrorKind(t, err, ErrorInvalidIdentifier)
}

func TestBuildIdentifiersAllowNumericLeadingZeroesAndNoIdentifierBound(t *testing.T) {
	build := "1.0.0+" + strings.Repeat("0001.", 40) + "0001"
	if _, err := Parse(build); err != nil {
		t.Fatalf("Parse(build identifiers): %v", err)
	}
}

func TestVersionInputByteBound(t *testing.T) {
	maximum := strings.Repeat("1", MaxVersionBytes-4) + ".0.0"
	if len(maximum) != MaxVersionBytes {
		t.Fatalf("test setup: maximum has %d bytes", len(maximum))
	}
	if _, err := Parse(maximum); err != nil {
		t.Fatalf("Parse(%d-byte version): %v", MaxVersionBytes, err)
	}

	oversized := strings.Repeat("1", MaxVersionBytes+1)
	_, err := Parse(oversized)
	assertErrorKind(t, err, ErrorTooLong)
	var parseErr *Error
	if !errors.As(err, &parseErr) {
		t.Fatalf("error type = %T, want *Error", err)
	}
	if parseErr.Bytes() != MaxVersionBytes+1 {
		t.Errorf("Bytes() = %d, want %d", parseErr.Bytes(), MaxVersionBytes+1)
	}
	if parseErr.Limit() != MaxVersionBytes {
		t.Errorf("Limit() = %d, want %d", parseErr.Limit(), MaxVersionBytes)
	}
}

func TestDecideAutomaticUpdate(t *testing.T) {
	tests := map[string]struct {
		current string
		remote  RemoteRelease
		channel Channel
		want    DecisionKind
		version string
	}{
		"stable rejects nightly": {
			current: "1.0.0", remote: release("v1.1.0-nightly.1", NightlyRelease),
			channel: StableChannel, want: DecisionChannelMismatch,
		},
		"nightly accepts newer prerelease": {
			current: "1.0.0", remote: release("v1.1.0-nightly.1", NightlyRelease),
			channel: NightlyChannel, want: DecisionAvailable, version: "1.1.0-nightly.1",
		},
		"nightly accepts later nightly": {
			current: "1.1.0-nightly.1", remote: release("v1.1.0-nightly.2", NightlyRelease),
			channel: NightlyChannel, want: DecisionAvailable, version: "1.1.0-nightly.2",
		},
		"nightly accepts final promotion": {
			current: "1.1.0-nightly.1", remote: release("v1.1.0", StableRelease),
			channel: NightlyChannel, want: DecisionAvailable, version: "1.1.0",
		},
		"nightly metadata needs prerelease": {
			current: "1.0.0", remote: release("v1.1.0+nightly.20260729", NightlyRelease),
			channel: StableChannel, want: DecisionInvalidRemoteMetadata,
		},
		"stable metadata needs final": {
			current: "1.0.0", remote: release("v1.1.0-rc.1", StableRelease),
			channel: NightlyChannel, want: DecisionInvalidRemoteMetadata,
		},
		"equal precedence with different builds": {
			current: "1.2.3+local", remote: release("v1.2.3+remote", StableRelease),
			channel: StableChannel, want: DecisionCurrent,
		},
		"older remote": {
			current: "2.0.0", remote: release("v1.99.99", StableRelease),
			channel: StableChannel, want: DecisionCurrent,
		},
		"empty development build": {
			current: "", remote: release("v999.0.0", StableRelease),
			channel: StableChannel, want: DecisionDevelopmentBuild,
		},
		"lowercase development build": {
			current: "dev", remote: release("v999.0.0", StableRelease),
			channel: StableChannel, want: DecisionDevelopmentBuild,
		},
		"uppercase development build": {
			current: "DEV", remote: release("v999.0.0", StableRelease),
			channel: StableChannel, want: DecisionDevelopmentBuild,
		},
		"mixed-case development build": {
			current: "DeV", remote: release("v999.0.0", StableRelease),
			channel: StableChannel, want: DecisionDevelopmentBuild,
		},
		"non-ASCII case fold is not development": {
			current: "ſev", remote: release("v999.0.0", StableRelease),
			channel: StableChannel, want: DecisionInvalidCurrentVersion,
		},
		"invalid current": {
			current: "not-a-version", remote: release("v2.0.0", StableRelease),
			channel: StableChannel, want: DecisionInvalidCurrentVersion,
		},
		"invalid remote": {
			current: "1.0.0", remote: release("latest", StableRelease),
			channel: StableChannel, want: DecisionInvalidRemoteVersion,
		},
	}

	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			got := DecideAutomaticUpdate(tc.current, tc.remote, tc.channel)
			if got.Kind() != tc.want {
				t.Fatalf("Kind() = %s, want %s", got.Kind(), tc.want)
			}
			available, ok := got.Version()
			if ok != (tc.want == DecisionAvailable) {
				t.Fatalf("Version() available = %t, want %t", ok, tc.want == DecisionAvailable)
			}
			if ok && available.String() != tc.version {
				t.Errorf("Version().String() = %q, want %q", available.String(), tc.version)
			}
			if (got.Err() != nil) != (tc.want == DecisionInvalidCurrentVersion || tc.want == DecisionInvalidRemoteVersion) {
				t.Errorf("Err() = %v for decision %s", got.Err(), got.Kind())
			}
		})
	}
}

func TestDecisionValidationOrder(t *testing.T) {
	tests := map[string]struct {
		current string
		remote  RemoteRelease
		want    DecisionKind
	}{
		"development ignores malformed remote": {
			current: "dev", remote: release("bad", NightlyRelease), want: DecisionDevelopmentBuild,
		},
		"invalid current precedes malformed remote": {
			current: "bad", remote: release("also-bad", NightlyRelease), want: DecisionInvalidCurrentVersion,
		},
		"invalid remote precedes channel mismatch": {
			current: "1.0.0", remote: release("bad", NightlyRelease), want: DecisionInvalidRemoteVersion,
		},
		"metadata precedes channel mismatch": {
			current: "1.0.0", remote: release("v2.0.0", NightlyRelease), want: DecisionInvalidRemoteMetadata,
		},
	}

	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			got := DecideAutomaticUpdate(tc.current, tc.remote, StableChannel)
			if got.Kind() != tc.want {
				t.Errorf("Kind() = %s, want %s", got.Kind(), tc.want)
			}
		})
	}
}

func TestVersionErrors(t *testing.T) {
	tests := map[string]struct {
		err      *Error
		want     string
		goString string
	}{
		"empty": {
			err: newError(ErrorEmpty), want: "version is empty", goString: "Empty",
		},
		"too long": {
			err:  newBoundedError(ErrorTooLong, 257, 256),
			want: "version is 257 bytes; limit is 256", goString: "TooLong { bytes: 257, limit: 256 }",
		},
		"invalid syntax": {
			err: newError(ErrorInvalidSyntax), want: "version syntax is invalid", goString: "InvalidSyntax",
		},
		"invalid core": {
			err:  newError(ErrorInvalidCore),
			want: "version must contain decimal major, minor, and patch", goString: "InvalidCore",
		},
		"leading zero": {
			err:  newError(ErrorLeadingZero),
			want: "numeric version identifier has a leading zero", goString: "LeadingZero",
		},
		"invalid identifier": {
			err:  newError(ErrorInvalidIdentifier),
			want: "prerelease or build identifier is invalid", goString: "InvalidIdentifier",
		},
		"too many prerelease identifiers": {
			err:      newBoundedError(ErrorTooManyPrereleaseIdentifiers, 0, 64),
			want:     "prerelease has more than 64 identifiers",
			goString: "TooManyPrereleaseIdentifiers { limit: 64 }",
		},
	}

	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			if got := tc.err.Error(); got != tc.want {
				t.Errorf("Error() = %q, want %q", got, tc.want)
			}
			if got := fmt.Sprintf("%#v", tc.err); got != tc.goString {
				t.Errorf("GoString() = %q, want %q", got, tc.goString)
			}
		})
	}
}

func assertErrorKind(t *testing.T, err error, want ErrorKind) {
	t.Helper()
	if err == nil {
		t.Fatalf("error = nil, want kind %d", want)
	}
	var parseErr *Error
	if !errors.As(err, &parseErr) {
		t.Fatalf("error type = %T, want *Error", err)
	}
	if parseErr.Kind() != want {
		t.Errorf("error kind = %d, want %d (%v)", parseErr.Kind(), want, err)
	}
}

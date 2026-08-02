package overlay

import (
	"strings"
	"unicode"
	"unicode/utf8"
)

// StripANSI removes CSI, OSC (terminated by BEL or ST), and other escape
// sequences, returning the printable text (UI-003). Plain control characters
// such as \t, \r and \n are kept; StringWidth assigns them a width.
func StripANSI(s string) string {
	if strings.IndexByte(s, 0x1b) < 0 {
		return s
	}
	var b strings.Builder
	b.Grow(len(s))
	for i := 0; i < len(s); {
		if s[i] == 0x1b {
			i = scanEscape(s, i)
			continue
		}
		b.WriteByte(s[i])
		i++
	}
	return b.String()
}

// StringWidth returns the terminal cell width of s, ignoring ANSI sequences
// and treating East-Asian wide/fullwidth characters and emoji as 2 cells
// (UI-003). Tabs advance to the next multiple of 8; other control characters
// count as 0.
func StringWidth(s string) int {
	s = StripANSI(s)
	w := 0
	for i := 0; i < len(s); {
		size, cw := nextCluster(s, i)
		i += size
		if cw < 0 { // tab
			w += 8 - w%8
			continue
		}
		w += cw
	}
	return w
}

// Truncate shortens s (plain text, no ANSI sequences) to at most width cells,
// appending "…" when truncated. Wide characters are never split.
func Truncate(s string, width int) string {
	if width <= 0 {
		return ""
	}
	if StringWidth(s) <= width {
		return s
	}
	budget := width - 1 // room for the ellipsis
	var b strings.Builder
	b.Grow(width + 4)
	used := 0
	for i := 0; i < len(s); {
		size, cw := nextCluster(s, i)
		if cw < 0 { // tab
			cw = 8 - used%8
		}
		if cw > 0 && used+cw > budget {
			break
		}
		b.WriteString(s[i : i+size])
		used += cw
		i += size
	}
	b.WriteString("…")
	return b.String()
}

// padRight appends spaces so s occupies exactly width cells.
func padRight(s string, width int) string {
	if n := width - StringWidth(s); n > 0 {
		return s + strings.Repeat(" ", n)
	}
	return s
}

// truncateANSI shortens a styled string to at most maxCells printable cells,
// copying ANSI escape sequences verbatim and never splitting a wide
// character or a grapheme cluster.
func truncateANSI(s string, maxCells int) string {
	if maxCells <= 0 {
		return ""
	}
	if StringWidth(s) <= maxCells {
		return s
	}
	var b strings.Builder
	b.Grow(len(s))
	used := 0
	for i := 0; i < len(s); {
		if s[i] == 0x1b {
			j := scanEscape(s, i)
			b.WriteString(s[i:j])
			i = j
			continue
		}
		size, cw := nextCluster(s, i)
		if cw < 0 { // tab
			cw = 8 - used%8
		}
		if used+cw > maxCells {
			break
		}
		b.WriteString(s[i : i+size])
		used += cw
		i += size
	}
	return b.String()
}

// prefixFoldLen returns the byte length of the prefix of s that matches
// prefix case-insensitively, or 0 when prefix is empty or does not match.
func prefixFoldLen(s, prefix string) int {
	if prefix == "" {
		return 0
	}
	i := 0
	for _, pr := range prefix {
		if i >= len(s) {
			return 0
		}
		sr, size := utf8.DecodeRuneInString(s[i:])
		if !foldEqual(sr, pr) {
			return 0
		}
		i += size
	}
	return i
}

func foldEqual(a, b rune) bool {
	return a == b ||
		unicode.ToLower(a) == unicode.ToLower(b) ||
		unicode.ToUpper(a) == unicode.ToUpper(b)
}

// scanEscape returns the index just past the escape sequence starting at
// s[i], which must be ESC. Unterminated string sequences (OSC/DCS/...)
// consume the rest of the string.
func scanEscape(s string, i int) int {
	j := i + 1
	if j >= len(s) {
		return j
	}
	c := s[j]
	switch {
	case c == '[': // CSI: parameter/intermediate bytes then final byte
		j++
		for j < len(s) {
			b := s[j]
			j++
			if b >= 0x40 && b <= 0x7e {
				break
			}
		}
		return j
	case c == ']', c == 'P', c == 'X', c == '^', c == '_': // OSC/DCS/SOS/PM/APC
		j++
		for j < len(s) {
			if s[j] == 0x07 { // BEL
				return j + 1
			}
			if s[j] == 0x1b && j+1 < len(s) && s[j+1] == '\\' { // ST
				return j + 2
			}
			j++
		}
		return len(s)
	case c >= 0x20 && c <= 0x2f: // intermediate bytes then one final byte
		for j < len(s) && s[j] >= 0x20 && s[j] <= 0x2f {
			j++
		}
		if j < len(s) {
			j++
		}
		return j
	default: // ESC + single byte (ESC 7, ESC 8, ESC M, ...)
		return j + 1
	}
}

// nextCluster returns the byte length and cell width of the grapheme-ish
// cluster starting at s[i]. A width of -1 marks a tab, whose advance depends
// on the current column. Combining marks, variation selectors, tag
// characters and zero-width joiners extend the current cluster; a ZWJ glues
// the following base character into the same cluster (emoji sequences).
func nextCluster(s string, i int) (size, width int) {
	r, sz := utf8.DecodeRuneInString(s[i:])
	switch {
	case r == utf8.RuneError && sz == 1:
		return 1, 1 // invalid byte renders as one replacement cell
	case r == '\t':
		return sz, -1
	case r < 0x20 || r == 0x7f:
		return sz, 0
	case r == 0x200d || isZeroWidth(r):
		return sz, 0
	case isRegionalIndicator(r):
		// A run of regional indicators renders as 2 cells per flag pair;
		// a lone indicator renders as a 2-cell boxed letter.
		n := 0
		j := i
		for j < len(s) {
			r2, sz2 := utf8.DecodeRuneInString(s[j:])
			if !isRegionalIndicator(r2) {
				break
			}
			n++
			j += sz2
		}
		return j - i, 2 * ((n + 1) / 2)
	}

	w := runeWidth(r)
	hasVS16 := false
	j := i + sz
	for j < len(s) {
		r2, sz2 := utf8.DecodeRuneInString(s[j:])
		if r2 == 0x200d {
			j += sz2
			if j < len(s) {
				r3, sz3 := utf8.DecodeRuneInString(s[j:])
				if w2 := runeWidth(r3); w2 > w {
					w = w2
				}
				if r3 == 0xfe0f {
					hasVS16 = true
				}
				j += sz3
			}
			continue
		}
		if r2 == 0xfe0f {
			hasVS16 = true
		}
		if isZeroWidth(r2) {
			j += sz2
			continue
		}
		break
	}
	if hasVS16 && w < 2 && isEmojiTextBase(r) {
		w = 2
	}
	return j - i, w
}

// runeWidth classifies a single base rune: 0 for control/combining/zero-width
// characters, 2 for East-Asian wide/fullwidth and emoji, 1 otherwise.
func runeWidth(r rune) int {
	switch {
	case r < 0x20 || (r >= 0x7f && r < 0xa0):
		return 0
	case r == 0x200b || r == 0x200c || r == 0x2060 || r == 0xfeff:
		return 0
	case isZeroWidth(r):
		return 0
	case inRanges(wideRanges, r), inRanges(emojiRanges, r):
		return 2
	}
	return 1
}

func isRegionalIndicator(r rune) bool { return r >= 0x1f1e6 && r <= 0x1f1ff }

// isEmojiTextBase reports whether r is a text-presentation character that
// becomes a 2-cell emoji when followed by U+FE0F.
func isEmojiTextBase(r rune) bool {
	return (r >= 0x2300 && r <= 0x23ff) ||
		(r >= 0x2600 && r <= 0x27bf) ||
		(r >= 0x2b00 && r <= 0x2bff) ||
		(r >= 0x1f000 && r <= 0x1f0ff)
}

func isZeroWidth(r rune) bool { return inRanges(zeroWidthRanges, r) }

func inRanges(rs [][2]rune, r rune) bool {
	lo, hi := 0, len(rs)-1
	for lo <= hi {
		mid := (lo + hi) / 2
		switch {
		case r < rs[mid][0]:
			hi = mid - 1
		case r > rs[mid][1]:
			lo = mid + 1
		default:
			return true
		}
	}
	return false
}

// zeroWidthRanges holds combining marks, variation selectors, Hangul
// vowel/trailing jamo, emoji modifiers and tag characters. Sorted by start.
var zeroWidthRanges = [][2]rune{
	{0x0300, 0x036f}, // combining diacritical marks
	{0x0483, 0x0489},
	{0x0591, 0x05bd},
	{0x05bf, 0x05bf},
	{0x05c1, 0x05c2},
	{0x05c4, 0x05c5},
	{0x05c7, 0x05c7},
	{0x0610, 0x061a},
	{0x064b, 0x065f},
	{0x0670, 0x0670},
	{0x06d6, 0x06dc},
	{0x06df, 0x06e4},
	{0x06e7, 0x06e8},
	{0x06ea, 0x06ed},
	{0x0e31, 0x0e31}, // Thai marks
	{0x0e34, 0x0e3a},
	{0x0e47, 0x0e4e},
	{0x1160, 0x11ff}, // Hangul jungseong/jongseong jamo
	{0x1ab0, 0x1aff},
	{0x1dc0, 0x1dff},
	{0x20d0, 0x20ff}, // combining marks for symbols
	{0xd7b0, 0xd7ff}, // Hangul jamo extended-B
	{0xfe00, 0xfe0f}, // variation selectors 1-16
	{0xfe20, 0xfe2f},
	{0xe0020, 0xe007f}, // tag characters
	{0xe0100, 0xe01ef}, // variation selectors 17-256
	{0x1f3fb, 0x1f3ff}, // emoji skin tone modifiers
}

// wideRanges holds East-Asian Wide and Fullwidth characters plus characters
// with default emoji presentation below U+1F300. Sorted by start.
var wideRanges = [][2]rune{
	{0x1100, 0x115f}, // Hangul choseong jamo
	{0x231a, 0x231b}, // watch, hourglass
	{0x2329, 0x232a},
	{0x23e9, 0x23ec},
	{0x23f0, 0x23f0},
	{0x23f3, 0x23f3},
	{0x25fd, 0x25fe},
	{0x2614, 0x2615},
	{0x2648, 0x2653},
	{0x267f, 0x267f},
	{0x2693, 0x2693},
	{0x26a1, 0x26a1},
	{0x26aa, 0x26ab},
	{0x26bd, 0x26be},
	{0x26c4, 0x26c5},
	{0x26ce, 0x26ce},
	{0x26d4, 0x26d4},
	{0x26ea, 0x26ea},
	{0x26f2, 0x26f3},
	{0x26f5, 0x26f5},
	{0x26fa, 0x26fa},
	{0x26fd, 0x26fd},
	{0x2705, 0x2705},
	{0x270a, 0x270b},
	{0x2728, 0x2728},
	{0x274c, 0x274c},
	{0x274e, 0x274e},
	{0x2753, 0x2755},
	{0x2757, 0x2757},
	{0x2795, 0x2797},
	{0x27b0, 0x27b0},
	{0x27bf, 0x27bf},
	{0x2b1b, 0x2b1c},
	{0x2b50, 0x2b50},
	{0x2b55, 0x2b55},
	{0x2e80, 0x303e}, // CJK radicals and ideographic punctuation
	{0x3041, 0x33ff}, // Hiragana, Katakana, CJK symbols
	{0x3400, 0x4dbf}, // CJK ext A
	{0x4e00, 0x9fff}, // CJK unified ideographs
	{0xa000, 0xa4cf}, // Yi
	{0xa960, 0xa97f}, // Hangul jamo extended-A
	{0xac00, 0xd7a3}, // Hangul syllables
	{0xf900, 0xfaff}, // CJK compatibility ideographs
	{0xfe10, 0xfe19},
	{0xfe30, 0xfe52},
	{0xfe54, 0xfe66},
	{0xfe68, 0xfe6b},
	{0xff00, 0xff60}, // fullwidth forms
	{0xffe0, 0xffe6},
	{0x16fe0, 0x16fe4},
	{0x17000, 0x187f7}, // Tangut
	{0x18800, 0x18cd5},
	{0x18d00, 0x18d08},
	{0x1aff0, 0x1aff3},
	{0x1aff5, 0x1affb},
	{0x1affd, 0x1affe},
	{0x1b000, 0x1b152}, // Kana supplement
	{0x1b164, 0x1b167},
	{0x1b170, 0x1b2fb}, // Nushu
	{0x1f004, 0x1f004},
	{0x1f0cf, 0x1f0cf},
	{0x1f18e, 0x1f18e},
	{0x1f191, 0x1f19a},
	{0x1f200, 0x1f202},
	{0x1f210, 0x1f23b},
	{0x1f240, 0x1f248},
	{0x1f250, 0x1f251},
	{0x1f260, 0x1f265},
	{0x20000, 0x2fffd}, // CJK ext B and beyond
	{0x30000, 0x3fffd},
}

// emojiRanges holds characters with default emoji presentation, which
// terminals render 2 cells wide. Regional indicators and skin tone modifiers
// are excluded and handled separately. Sorted by start.
var emojiRanges = [][2]rune{
	{0x1f000, 0x1f1e5},
	{0x1f200, 0x1f64f},
	{0x1f680, 0x1f6ff},
	{0x1f900, 0x1f9ff},
	{0x1fa70, 0x1faff},
}

package completion

import (
	"cmp"
	"math"
	"slices"
	"strings"
	"unicode"
)

// MergeSuggestions deduplicates candidates by complete insertion result and
// merges metadata. Invalid edits, blank displays, blank results, and results
// identical to the trim-end-normalized query are discarded. No ranking or UI
// limit is applied.
func MergeSuggestions(query CompletionQuery, batches ...[]Suggestion) []Suggestion {
	prepared := make([]preparedCandidate, 0)
	for _, batch := range batches {
		for _, candidate := range batch {
			if value, ok := prepareCandidate(query, candidate); ok {
				prepared = append(prepared, value)
			}
		}
	}
	if len(prepared) == 0 {
		return nil
	}

	queryBytes := []byte(query.line)
	forward := newLCEIndex(queryBytes)
	reversed := make([]byte, len(queryBytes))
	for index := range queryBytes {
		reversed[len(queryBytes)-1-index] = queryBytes[index]
	}
	reverse := newLCEIndex(reversed)
	prefixTrim := newPrefixTrimIndex(query.line)
	normalizedQueryLen := len(strings.TrimRightFunc(query.line, unicode.IsSpace))
	grouped := make(map[resultKey][]Suggestion)

	for _, candidate := range prepared {
		key := newResultKey(queryBytes, candidate, forward, reverse)
		normalizedLen := candidate.normalizedResultLen(
			len(query.line), normalizedQueryLen, prefixTrim,
		)
		if normalizedLen == 0 ||
			(normalizedLen == normalizedQueryLen && key.commonPrefix >= normalizedQueryLen) {
			continue
		}
		grouped[key] = append(grouped[key], candidate.suggestion)
	}

	keys := make([]resultKey, 0, len(grouped))
	for key := range grouped {
		keys = append(keys, key)
	}
	slices.SortFunc(keys, compareResultKeys)
	merged := make([]Suggestion, 0, len(keys))
	for _, key := range keys {
		duplicates := grouped[key]
		slices.SortFunc(duplicates, compareCandidates)
		candidate := duplicates[0]
		for _, duplicate := range duplicates[1:] {
			candidate.mergeMetadata(duplicate)
		}
		merged = append(merged, compactCandidate(candidate))
	}
	return slices.Clip(merged)
}

type preparedCandidate struct {
	suggestion          Suggestion
	resolvedReplacement string
}

func prepareCandidate(query CompletionQuery, candidate Suggestion) (preparedCandidate, bool) {
	if strings.TrimSpace(candidate.display) == "" {
		return preparedCandidate{}, false
	}
	edit := candidate.edit
	if edit.start < 0 || edit.start > edit.end ||
		!utf8Boundary(query.line, edit.start) || !utf8Boundary(query.line, edit.end) {
		return preparedCandidate{}, false
	}
	candidate = compactCandidate(candidate)
	resolved, err := candidate.ResolvedEdit(query.line)
	if err != nil {
		return preparedCandidate{}, false
	}
	return preparedCandidate{
		suggestion: candidate, resolvedReplacement: strings.Clone(resolved.replacement),
	}, true
}

func (c preparedCandidate) normalizedResultLen(
	queryLen, normalizedQueryLen int,
	prefixTrim prefixTrimIndex,
) int {
	edit := c.suggestion.edit
	resultLen := edit.start + len(c.resolvedReplacement) + queryLen - edit.end
	if edit.end < normalizedQueryLen {
		return resultLen - (queryLen - normalizedQueryLen)
	}
	replacementLen := len(strings.TrimRightFunc(c.resolvedReplacement, unicode.IsSpace))
	if replacementLen == 0 {
		return prefixTrim.at(edit.start)
	}
	return edit.start + replacementLen
}

type resultKey struct {
	commonPrefix     int
	querySuffixStart int
	changed          string
}

func newResultKey(
	query []byte,
	candidate preparedCandidate,
	forward, reverse lceIndex,
) resultKey {
	edit := candidate.suggestion.edit
	replacement := []byte(candidate.resolvedReplacement)
	commonPrefix := resultCommonPrefix(query, edit.start, edit.end, replacement, forward)
	resultLen := edit.start + len(replacement) + len(query) - edit.end
	rawSuffix := resultCommonSuffix(query, edit.start, edit.end, replacement, reverse)
	commonSuffix := min(rawSuffix, len(query)-min(commonPrefix, len(query)))
	commonSuffix = min(commonSuffix, resultLen-min(commonPrefix, resultLen))
	resultMiddleEnd := resultLen - commonSuffix
	changed := copyResultRange(
		query, edit.start, edit.end, replacement, commonPrefix, resultMiddleEnd,
	)
	return resultKey{
		commonPrefix:     commonPrefix,
		querySuffixStart: len(query) - commonSuffix,
		changed:          string(changed),
	}
}

func compareResultKeys(left, right resultKey) int {
	if order := cmp.Compare(left.commonPrefix, right.commonPrefix); order != 0 {
		return order
	}
	if order := cmp.Compare(left.querySuffixStart, right.querySuffixStart); order != 0 {
		return order
	}
	return strings.Compare(left.changed, right.changed)
}

func resultCommonPrefix(
	query []byte,
	editStart, editEnd int,
	replacement []byte,
	forward lceIndex,
) int {
	queryTail := query[editStart:]
	replacementMatch := commonPrefixLen(queryTail, replacement)
	if replacementMatch < min(len(queryTail), len(replacement)) {
		return editStart + replacementMatch
	}
	if replacementMatch == len(queryTail) {
		return len(query)
	}
	queryAfterReplacement := editStart + len(replacement)
	return queryAfterReplacement + forward.lcp(queryAfterReplacement, editEnd)
}

func resultCommonSuffix(
	query []byte,
	editStart, editEnd int,
	replacement []byte,
	reverse lceIndex,
) int {
	unchangedSuffix := len(query) - editEnd
	queryPrefix := query[:editEnd]
	replacementMatch := commonSuffixLen(queryPrefix, replacement)
	if replacementMatch < min(len(queryPrefix), len(replacement)) {
		return unchangedSuffix + replacementMatch
	}
	if replacementMatch == len(queryPrefix) {
		return len(query)
	}
	queryBeforeReplacement := editEnd - len(replacement)
	reversedLeft := len(query) - editStart
	reversedRight := len(query) - queryBeforeReplacement
	return unchangedSuffix + len(replacement) + reverse.lcp(reversedLeft, reversedRight)
}

func commonPrefixLen(left, right []byte) int {
	limit := min(len(left), len(right))
	for index := range limit {
		if left[index] != right[index] {
			return index
		}
	}
	return limit
}

func commonSuffixLen(left, right []byte) int {
	limit := min(len(left), len(right))
	for index := range limit {
		if left[len(left)-1-index] != right[len(right)-1-index] {
			return index
		}
	}
	return limit
}

func copyResultRange(
	query []byte,
	editStart, editEnd int,
	replacement []byte,
	start, end int,
) []byte {
	if start >= end {
		return nil
	}
	changed := make([]byte, 0, end-start)
	changed = copyOverlap(changed, replacement, editStart, start, end)
	changed = copyOverlap(
		changed, query[editEnd:], editStart+len(replacement), start, end,
	)
	return changed
}

func copyOverlap(
	output, segment []byte,
	segmentStart, rangeStart, rangeEnd int,
) []byte {
	segmentEnd := segmentStart + len(segment)
	overlapStart := max(rangeStart, segmentStart)
	overlapEnd := min(rangeEnd, segmentEnd)
	if overlapStart < overlapEnd {
		output = append(output, segment[overlapStart-segmentStart:overlapEnd-segmentStart]...)
	}
	return output
}

type lceIndex struct {
	length       int
	suffixRank   []int
	rangeMinimum []int
	treeBase     int
}

func newLCEIndex(data []byte) lceIndex {
	if len(data) == 0 {
		return lceIndex{rangeMinimum: []int{math.MaxInt, math.MaxInt}, treeBase: 1}
	}
	length := len(data)
	suffixes := make([]int, length)
	scratch := make([]int, length)
	ranks := make([]int, length)
	newRanks := make([]int, length)
	for index, char := range data {
		suffixes[index] = index
		ranks[index] = int(char) + 1
	}
	counts := make([]int, max(length, 257)+1)
	maxRank := 256
	for offset := 1; ; offset = saturatedDouble(offset) {
		stableCountingSort(suffixes, scratch, counts, maxRank, func(suffix int) int {
			if suffix <= length-1-offset {
				return ranks[suffix+offset]
			}
			return 0
		})
		stableCountingSort(scratch, suffixes, counts, maxRank, func(suffix int) int {
			return ranks[suffix]
		})
		classes := 1
		newRanks[suffixes[0]] = classes
		for index := 1; index < len(suffixes); index++ {
			if suffixRankPair(ranks, suffixes[index-1], offset) !=
				suffixRankPair(ranks, suffixes[index], offset) {
				classes++
			}
			newRanks[suffixes[index]] = classes
		}
		ranks, newRanks = newRanks, ranks
		maxRank = classes
		if classes == length {
			break
		}
	}

	suffixRank := make([]int, length)
	for rank, suffix := range suffixes {
		suffixRank[suffix] = rank
	}
	adjacent := adjacentLCP(data, suffixes, suffixRank)
	treeBase := 1
	for treeBase < length {
		treeBase *= 2
	}
	rangeMinimum := make([]int, treeBase*2)
	for index := range rangeMinimum {
		rangeMinimum[index] = math.MaxInt
	}
	copy(rangeMinimum[treeBase:treeBase+length], adjacent)
	for index := treeBase - 1; index >= 1; index-- {
		rangeMinimum[index] = min(rangeMinimum[index*2], rangeMinimum[index*2+1])
	}
	return lceIndex{
		length: length, suffixRank: suffixRank,
		rangeMinimum: rangeMinimum, treeBase: treeBase,
	}
}

func (index lceIndex) lcp(left, right int) int {
	if left == right {
		return max(0, index.length-left)
	}
	if left < 0 || right < 0 || left >= index.length || right >= index.length {
		return 0
	}
	leftRank := index.suffixRank[left]
	rightRank := index.suffixRank[right]
	rangeStart := min(leftRank, rightRank) + 1 + index.treeBase
	rangeEnd := max(leftRank, rightRank) + 1 + index.treeBase
	minimum := math.MaxInt
	for rangeStart < rangeEnd {
		if rangeStart%2 == 1 {
			minimum = min(minimum, index.rangeMinimum[rangeStart])
			rangeStart++
		}
		if rangeEnd%2 == 1 {
			rangeEnd--
			minimum = min(minimum, index.rangeMinimum[rangeEnd])
		}
		rangeStart /= 2
		rangeEnd /= 2
	}
	return minimum
}

func stableCountingSort(
	input, output, counts []int,
	maxKey int,
	key func(int) int,
) {
	clear(counts[:maxKey+1])
	for _, value := range input {
		counts[key(value)]++
	}
	position := 0
	for index := 0; index <= maxKey; index++ {
		size := counts[index]
		counts[index] = position
		position += size
	}
	for _, value := range input {
		valueKey := key(value)
		output[counts[valueKey]] = value
		counts[valueKey]++
	}
}

type rankPair struct{ first, second int }

func suffixRankPair(ranks []int, suffix, offset int) rankPair {
	second := 0
	if suffix <= len(ranks)-1-offset {
		second = ranks[suffix+offset]
	}
	return rankPair{first: ranks[suffix], second: second}
}

func adjacentLCP(data []byte, suffixes, suffixRank []int) []int {
	adjacent := make([]int, len(data))
	matched := 0
	for left := range data {
		rank := suffixRank[left]
		if rank == 0 {
			matched = 0
			continue
		}
		right := suffixes[rank-1]
		for left+matched < len(data) && right+matched < len(data) &&
			data[left+matched] == data[right+matched] {
			matched++
		}
		adjacent[rank] = matched
		matched = max(0, matched-1)
	}
	return adjacent
}

type prefixTrimIndex struct{ trimmedLen []int }

func newPrefixTrimIndex(value string) prefixTrimIndex {
	trimmedLen := make([]int, len(value)+1)
	lastNonWhitespace := 0
	for start, character := range value {
		end := start + len(string(character))
		if !unicode.IsSpace(character) {
			lastNonWhitespace = end
		}
		trimmedLen[end] = lastNonWhitespace
	}
	return prefixTrimIndex{trimmedLen: trimmedLen}
}

func (index prefixTrimIndex) at(position int) int { return index.trimmedLen[position] }

func compareCandidates(left, right Suggestion) int {
	if order := strings.Compare(left.identity, right.identity); order != 0 {
		return order
	}
	for _, values := range [][2]int{
		{left.edit.start, right.edit.start},
		{left.edit.end, right.edit.end},
	} {
		if order := cmp.Compare(values[0], values[1]); order != 0 {
			return order
		}
	}
	if order := strings.Compare(left.edit.replacement, right.edit.replacement); order != 0 {
		return order
	}
	if order := cmp.Compare(left.insertion, right.insertion); order != 0 {
		return order
	}
	if order := cmp.Compare(left.source, right.source); order != 0 {
		return order
	}
	if order := compareSources(left.sources, right.sources); order != 0 {
		return order
	}
	for _, values := range [][2]string{
		{left.display, right.display},
		{left.description, right.description},
		{left.icon, right.icon},
	} {
		if order := strings.Compare(values[0], values[1]); order != 0 {
			return order
		}
	}
	if order := totalFloatCompare(left.staticPriority, right.staticPriority); order != 0 {
		return order
	}
	return totalFloatCompare(left.confidence, right.confidence)
}

func compareSources(left, right []SuggestionSource) int {
	for index := 0; index < min(len(left), len(right)); index++ {
		if order := cmp.Compare(left[index], right[index]); order != 0 {
			return order
		}
	}
	return cmp.Compare(len(left), len(right))
}

func totalFloatCompare(left, right float64) int {
	leftBits := math.Float64bits(left)
	rightBits := math.Float64bits(right)
	leftKey := leftBits
	if leftBits>>63 != 0 {
		leftKey = ^leftBits
	} else {
		leftKey |= uint64(1) << 63
	}
	rightKey := rightBits
	if rightBits>>63 != 0 {
		rightKey = ^rightBits
	} else {
		rightKey |= uint64(1) << 63
	}
	return cmp.Compare(leftKey, rightKey)
}

func compactCandidate(candidate Suggestion) Suggestion {
	candidate.edit.replacement = strings.Clone(candidate.edit.replacement)
	candidate.display = strings.Clone(candidate.display)
	candidate.description = strings.Clone(candidate.description)
	candidate.icon = strings.Clone(candidate.icon)
	candidate.identity = strings.Clone(candidate.identity)
	candidate.sources = slices.Clip(candidate.sources)
	return candidate
}

func saturatedDouble(value int) int {
	if value > math.MaxInt/2 {
		return math.MaxInt
	}
	return value * 2
}

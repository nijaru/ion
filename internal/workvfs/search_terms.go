package workspace

import (
	"slices"
	"strings"
)

func corpusTerms(path string, data []byte) []string {
	return trigrams(strings.ToLower(path + "\n" + string(data)))
}

func queryTerms(query string) []string {
	return trigrams(strings.ToLower(query))
}

func trigrams(text string) []string {
	runes := []rune(text)
	if len(runes) < 3 {
		return nil
	}

	terms := make([]string, 0, len(runes)-2)
	seen := make(map[string]struct{}, len(runes)-2)
	for i := 0; i+3 <= len(runes); i++ {
		term := string(runes[i : i+3])
		if strings.TrimSpace(term) == "" {
			continue
		}
		if _, ok := seen[term]; ok {
			continue
		}
		seen[term] = struct{}{}
		terms = append(terms, term)
	}
	slices.Sort(terms)
	return terms
}

func scoreHit(query string, ref ContentRef, matchedTerms int) int {
	score := matchedTerms * 100
	if strings.Contains(strings.ToLower(ref.Path), query) {
		score += 25
	}
	score -= len(ref.Path)
	return score
}

func insertDocID(list []uint32, id uint32) []uint32 {
	idx, found := slices.BinarySearch(list, id)
	if found {
		return list
	}
	list = append(list, 0)
	copy(list[idx+1:], list[idx:])
	list[idx] = id
	return list
}

func removeDocID(list []uint32, id uint32) []uint32 {
	idx, found := slices.BinarySearch(list, id)
	if !found {
		return list
	}
	return slices.Delete(list, idx, idx+1)
}

func intersectSorted(a, b []uint32) []uint32 {
	if len(a) == 0 || len(b) == 0 {
		return nil
	}
	out := make([]uint32, 0, min(len(a), len(b)))
	i, j := 0, 0
	for i < len(a) && j < len(b) {
		switch {
		case a[i] == b[j]:
			out = append(out, a[i])
			i++
			j++
		case a[i] < b[j]:
			i++
		default:
			j++
		}
	}
	return out
}

package review

import "github.com/pmezard/go-difflib/difflib"

func Hunks(reviewed string, current string) []Hunk {
	a := splitLines(reviewed)
	b := splitLines(current)
	matcher := difflib.NewMatcher(a, b)
	groups := matcher.GetGroupedOpCodes(3)
	hunks := make([]Hunk, 0, len(groups))

	for _, group := range groups {
		hunk := Hunk{}
		for _, op := range group {
			switch op.Tag {
			case 'e':
				for _, line := range a[op.I1:op.I2] {
					hunk.Lines = append(hunk.Lines, DiffLine{Kind: DiffLineEqual, Text: line})
				}
			case 'd':
				for _, line := range a[op.I1:op.I2] {
					hunk.Lines = append(hunk.Lines, DiffLine{Kind: DiffLineDelete, Text: line})
				}
			case 'i':
				for _, line := range b[op.J1:op.J2] {
					hunk.Lines = append(hunk.Lines, DiffLine{Kind: DiffLineInsert, Text: line})
				}
			case 'r':
				for _, line := range a[op.I1:op.I2] {
					hunk.Lines = append(hunk.Lines, DiffLine{Kind: DiffLineDelete, Text: line})
				}
				for _, line := range b[op.J1:op.J2] {
					hunk.Lines = append(hunk.Lines, DiffLine{Kind: DiffLineInsert, Text: line})
				}
			}
		}
		if hunk.hasChanges() {
			hunks = append(hunks, hunk)
		}
	}

	return hunks
}

func ApplyHunk(reviewed string, hunk Hunk) string {
	output := ""
	hunkStarted := false
	oldLines := splitLines(reviewed)
	oldIdx := 0

	copyUntil := func(needle string) {
		for oldIdx < len(oldLines) && oldLines[oldIdx] != needle {
			output += oldLines[oldIdx]
			oldIdx++
		}
	}

	for _, line := range hunk.Lines {
		switch line.Kind {
		case DiffLineEqual:
			copyUntil(line.Text)
			output += line.Text
			if oldIdx < len(oldLines) {
				oldIdx++
			}
			hunkStarted = true
		case DiffLineDelete:
			copyUntil(line.Text)
			if oldIdx < len(oldLines) {
				oldIdx++
			}
			hunkStarted = true
		case DiffLineInsert:
			if !hunkStarted {
				hunkStarted = true
			}
			output += line.Text
		}
	}

	for oldIdx < len(oldLines) {
		output += oldLines[oldIdx]
		oldIdx++
	}

	return output
}

func splitLines(s string) []string {
	if s == "" {
		return nil
	}
	lines := []string{}
	start := 0
	for i, r := range s {
		if r == '\n' {
			lines = append(lines, s[start:i+1])
			start = i + 1
		}
	}
	if start < len(s) {
		lines = append(lines, s[start:])
	}
	return lines
}

func (h Hunk) hasChanges() bool {
	for _, line := range h.Lines {
		if line.Kind != DiffLineEqual {
			return true
		}
	}
	return false
}

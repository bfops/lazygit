package controllers

import (
	"testing"

	reviewmode "github.com/jesseduffield/lazygit/pkg/gui/modes/review"
	"github.com/jesseduffield/lazygit/pkg/gui/style"
	reviewcore "github.com/jesseduffield/lazygit/pkg/review"
	"github.com/stretchr/testify/assert"
)

func TestReviewDiffContentUsesDiffStyles(t *testing.T) {
	session := &reviewcore.Session{
		Files: []reviewcore.ReviewFile{
			{
				Meta: reviewcore.FileState{Path: "app.go"},
				Reviewed: `package main

func main() {
	old()
}
`,
				Current: `package main

func main() {
	new()
}
`,
			},
		},
	}

	content := reviewDiffContent(reviewmode.New(reviewmode.DiffViewModeWrap, 0), session)

	assert.Contains(t, content, "app.go 1 hunk remaining")
	assert.Contains(t, content, style.FgRed.Sprint("-\told()\n"))
	assert.Contains(t, content, style.FgGreen.Sprint("+\tnew()\n"))
}

func TestRemainingHunksLabelIncludesZero(t *testing.T) {
	assert.Equal(t, "0 hunks remaining", remainingHunksLabel(0))
	assert.Equal(t, "1 hunk remaining", remainingHunksLabel(1))
	assert.Equal(t, "2 hunks remaining", remainingHunksLabel(2))
}

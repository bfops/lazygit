package controllers

import (
	"testing"

	guicontext "github.com/jesseduffield/lazygit/pkg/gui/context"
	reviewmode "github.com/jesseduffield/lazygit/pkg/gui/modes/review"
	"github.com/jesseduffield/lazygit/pkg/gui/style"
	reviewcore "github.com/jesseduffield/lazygit/pkg/review"
	"github.com/stretchr/testify/assert"
)

func TestReviewDiffContentUsesDiffStyles(t *testing.T) {
	session := &reviewcore.Session{
		Files: []reviewcore.ReviewFile{
			{
				Meta: reviewcore.FileState{Path: "app.go", TotalHunkCount: 1},
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

	assert.Contains(t, content, "app.go "+guicontext.ReviewHunkProgressLabel(session.Files[0]))
	assert.Contains(t, content, style.FgRed.Sprint("-\told()\n"))
	assert.Contains(t, content, style.FgGreen.Sprint("+\tnew()\n"))
}

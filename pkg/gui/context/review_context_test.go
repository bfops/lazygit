package context

import (
	"testing"

	"github.com/jesseduffield/lazygit/pkg/gui/style"
	reviewcore "github.com/jesseduffield/lazygit/pkg/review"
	"github.com/stretchr/testify/assert"
)

func TestReviewHunkProgressLabel(t *testing.T) {
	assert.Equal(t,
		style.FgGreen.Sprint("All"),
		ReviewHunkProgressLabel(reviewcore.ReviewFile{}),
	)
	assert.Equal(t,
		style.FgYellow.Sprint("1/3 hunks reviewed"),
		ReviewHunkProgressLabel(reviewcore.ReviewFile{Meta: reviewcore.FileState{ReviewedHunkCount: 1, TotalHunkCount: 3}}),
	)
	assert.Equal(t,
		style.FgGreen.Sprint("3/3 hunks reviewed"),
		ReviewHunkProgressLabel(reviewcore.ReviewFile{Meta: reviewcore.FileState{ReviewedHunkCount: 3, TotalHunkCount: 3}}),
	)
}

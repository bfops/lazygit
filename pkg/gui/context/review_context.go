package context

import (
	"fmt"

	"github.com/jesseduffield/lazygit/pkg/gui/style"
	"github.com/jesseduffield/lazygit/pkg/gui/types"
	reviewcore "github.com/jesseduffield/lazygit/pkg/review"
)

type ReviewFileListItem struct {
	File reviewcore.ReviewFile
}

func (r *ReviewFileListItem) ID() string {
	return r.File.Meta.Path
}

func (r *ReviewFileListItem) URN() string {
	return "review-file-" + r.File.Meta.Path
}

type ReviewContext struct {
	*ListViewModel[*ReviewFileListItem]
	*ListContextTrait
	c *ContextCommon
}

var _ types.IListContext = (*ReviewContext)(nil)

func NewReviewContext(c *ContextCommon) *ReviewContext {
	viewModel := NewListViewModel(func() []*ReviewFileListItem {
		session := c.Model().ReviewSession
		if session == nil {
			return nil
		}

		items := make([]*ReviewFileListItem, len(session.Files))
		for i, file := range session.Files {
			items[i] = &ReviewFileListItem{File: file}
		}
		return items
	})

	getDisplayStrings := func(startIdx int, endIdx int) [][]string {
		items := viewModel.GetItems()
		lines := make([][]string, 0, endIdx-startIdx)
		for _, item := range items[startIdx:endIdx] {
			lines = append(lines, []string{
				item.File.Meta.Path,
				ReviewHunkProgressLabel(item.File),
			})
		}
		return lines
	}

	return &ReviewContext{
		ListViewModel: viewModel,
		ListContextTrait: &ListContextTrait{
			Context: NewSimpleContext(NewBaseContext(NewBaseContextOpts{
				View:       c.Views().Review,
				WindowName: "files",
				Key:        REVIEW_CONTEXT_KEY,
				Kind:       types.SIDE_CONTEXT,
				Focusable:  true,
			})),
			ListRenderer: ListRenderer{
				list:              viewModel,
				getDisplayStrings: getDisplayStrings,
			},
			c: c,
		},
		c: c,
	}
}

func (r *ReviewContext) HandleRender() {
	session := r.c.Model().ReviewSession
	title := "PR Review"
	if session != nil {
		title = fmt.Sprintf("PR Review #%d", session.Manifest.PR.Number)
	}
	r.GetView().Title = title
	r.ListContextTrait.HandleRender()
}

func ReviewHunkProgressLabel(file reviewcore.ReviewFile) string {
	reviewed := file.Meta.ReviewedHunkCount
	total := file.Meta.TotalHunkCount
	if total == 0 {
		return style.FgGreen.Sprint("All")
	}

	label := fmt.Sprintf("%d/%d hunks reviewed", reviewed, total)
	if reviewed >= total {
		return style.FgGreen.Sprint(label)
	}
	return style.FgYellow.Sprint(label)
}

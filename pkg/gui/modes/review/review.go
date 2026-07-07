package review

type DiffViewMode string

const (
	DiffViewModeWrap   DiffViewMode = "wrap"
	DiffViewModeScroll DiffViewMode = "scroll"
)

type Review struct {
	Active               bool
	FileIndex            int
	HunkIndex            int
	DiffVerticalOffset   int
	DiffHorizontalOffset int
	DiffViewMode         DiffViewMode
	Loading              bool
	PendingMarks         int
	Message              string
}

func New(diffViewMode DiffViewMode, diffHorizontalOffset int) Review {
	if diffViewMode == "" {
		diffViewMode = DiffViewModeWrap
	}
	return Review{DiffViewMode: diffViewMode, DiffHorizontalOffset: diffHorizontalOffset}
}

func (r *Review) ToggleDiffMode() {
	if r.DiffViewMode == DiffViewModeWrap {
		r.DiffViewMode = DiffViewModeScroll
		return
	}
	r.DiffViewMode = DiffViewModeWrap
}

func (r *Review) ResetSelection() {
	r.FileIndex = 0
	r.HunkIndex = 0
	r.DiffVerticalOffset = 0
	r.DiffHorizontalOffset = 0
}

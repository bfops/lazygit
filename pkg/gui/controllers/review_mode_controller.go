package controllers

import (
	"fmt"
	"strings"

	"github.com/jesseduffield/lazygit/pkg/gocui"
	"github.com/jesseduffield/lazygit/pkg/gui/context"
	reviewmode "github.com/jesseduffield/lazygit/pkg/gui/modes/review"
	"github.com/jesseduffield/lazygit/pkg/gui/style"
	"github.com/jesseduffield/lazygit/pkg/gui/types"
	reviewcore "github.com/jesseduffield/lazygit/pkg/review"
)

type ReviewController struct {
	baseController
	*ListControllerTrait[*context.ReviewFileListItem]
	c *ControllerCommon
}

var _ types.IController = &ReviewController{}

func NewReviewController(c *ControllerCommon) *ReviewController {
	return &ReviewController{
		baseController: baseController{},
		ListControllerTrait: NewListControllerTrait(
			c,
			c.Contexts().Review,
			c.Contexts().Review.GetSelected,
			c.Contexts().Review.GetSelectedItems,
		),
		c: c,
	}
}

func (r *ReviewController) GetOnFocus() func(types.OnFocusOpts) {
	return func(types.OnFocusOpts) {
		r.syncSelection()
	}
}

func (r *ReviewController) GetOnRenderToMain() func() {
	return func() {
		r.syncSelection()
		r.renderToMain()
	}
}

func (r *ReviewController) syncSelection() {
	session := r.c.Model().ReviewSession
	if session == nil || len(session.Files) == 0 {
		r.c.Modes().Review.FileIndex = 0
		r.c.Modes().Review.HunkIndex = 0
		r.c.Modes().Review.DiffVerticalOffset = 0
		return
	}

	selected := r.c.Contexts().Review.GetList().GetSelectedLineIdx()
	selected = clamp(selected, 0, len(session.Files)-1)
	if r.c.Modes().Review.FileIndex != selected {
		r.c.Modes().Review.FileIndex = selected
		r.c.Modes().Review.HunkIndex = 0
		r.c.Modes().Review.DiffVerticalOffset = 0
	}
}

func (r *ReviewController) renderToMain() {
	r.c.RenderToMainViews(types.RefreshMainOpts{
		Pair: r.c.MainViewPairs().Normal,
		Main: &types.ViewUpdateOpts{
			Title:    r.reviewMainTitle(),
			SubTitle: r.reviewDiffSubtitle(),
			Task:     types.NewRenderStringWithScrollTask(r.reviewDiffContent(), r.c.Modes().Review.DiffHorizontalOffset, r.c.Modes().Review.DiffVerticalOffset),
		},
	})
}

func (r *ReviewController) reviewMainTitle() string {
	session := r.c.Model().ReviewSession
	if session == nil || len(session.Files) == 0 {
		return "Review Hunk"
	}
	file := session.Files[clamp(r.c.Modes().Review.FileIndex, 0, len(session.Files)-1)]
	return fmt.Sprintf("%s - %s", file.Meta.Path, remainingHunksLabel(len(reviewcore.Hunks(file.Reviewed, file.Current))))
}

func (r *ReviewController) reviewDiffSubtitle() string {
	mode := r.c.Modes().Review
	if mode.DiffViewMode == reviewmode.DiffViewModeScroll {
		return fmt.Sprintf("scroll x=%d", mode.DiffHorizontalOffset)
	}
	return "wrap"
}

func (r *ReviewController) reviewDiffContent() string {
	return reviewDiffContent(r.c.Modes().Review, r.c.Model().ReviewSession)
}

func (self *GlobalController) toggleReviewMode() error {
	if self.c.Modes().Review.Active {
		self.c.Modes().Review.Active = false
		self.c.Model().ReviewSession = nil
		self.c.Model().ReviewLogs = nil
		self.c.Contexts().Review.HandleRender()
		self.c.Context().Push(self.c.Contexts().Files, types.OnFocusOpts{})
		self.c.Contexts().Files.HandleRenderToMain()
		self.c.Toast("Exited PR review mode")
		return nil
	}

	self.c.Modes().Review = reviewmode.New(
		reviewmode.DiffViewMode(self.c.GetAppState().Review.DiffViewMode),
		self.c.GetAppState().Review.DiffHorizontalOffset,
	)
	self.c.Modes().Review.Active = true
	self.c.Modes().Review.Loading = true
	self.c.Modes().Review.Message = "loading review"
	self.c.Model().ReviewLogs = []string{"loading review"}
	self.renderReview()
	self.c.Helpers().Window.MoveToTopOfWindow(self.c.Contexts().Review)
	self.c.Context().Push(self.c.Contexts().Review, types.OnFocusOpts{})

	self.c.OnWorker(func(gocui.Task) error {
		err := self.loadReview("")
		self.c.OnUIThread(func() error {
			self.c.Modes().Review.Loading = false
			if err != nil {
				self.c.Modes().Review.Message = "review load failed"
				self.appendReviewLog("review load failed: " + err.Error())
				self.c.ErrorToast(err.Error())
			} else {
				self.c.Modes().Review.Message = "review loaded"
				self.appendReviewLog("review loaded")
			}
			self.renderReview()
			return nil
		})
		return nil
	})

	return nil
}

func (self *GlobalController) refreshReview() error {
	if !self.c.Modes().Review.Active || self.c.Model().ReviewSession == nil {
		return nil
	}

	self.c.Modes().Review.Loading = true
	self.c.Modes().Review.Message = "refreshing review"
	self.appendReviewLog("refreshing review")
	self.renderReview()

	self.c.OnWorker(func(gocui.Task) error {
		backend := reviewcore.NewGh("")
		err := self.c.Model().ReviewSession.RefreshFiles(backend, func(message string) {
			self.c.OnUIThreadContentOnly(func() error {
				self.appendReviewLog(message)
				self.renderReview()
				return nil
			})
		})
		self.c.OnUIThread(func() error {
			self.c.Modes().Review.Loading = false
			if err != nil {
				self.c.Modes().Review.Message = "refresh failed"
				self.appendReviewLog("refresh failed: " + err.Error())
				self.c.ErrorToast(err.Error())
			} else {
				self.c.Modes().Review.Message = "refresh complete"
				self.appendReviewLog("refresh complete")
			}
			self.renderReview()
			return nil
		})
		return nil
	})

	return nil
}

func (self *GlobalController) reviewModeOnly() *types.DisabledReason {
	if self.c.Modes().Review.Active {
		return nil
	}
	return &types.DisabledReason{Text: "", AllowFurtherDispatching: true}
}

func (self *GlobalController) reviewNextHunk() error {
	hunks := self.currentReviewHunks()
	if len(hunks) == 0 {
		return nil
	}
	if self.c.Modes().Review.HunkIndex+1 < len(hunks) {
		self.c.Modes().Review.HunkIndex++
		self.c.Modes().Review.DiffVerticalOffset = 0
	}
	self.renderReview()
	return nil
}

func (self *GlobalController) reviewPrevHunk() error {
	if self.c.Modes().Review.HunkIndex > 0 {
		self.c.Modes().Review.HunkIndex--
		self.c.Modes().Review.DiffVerticalOffset = 0
	}
	self.renderReview()
	return nil
}

func (self *GlobalController) reviewNextFile() error {
	session := self.c.Model().ReviewSession
	if session == nil || len(session.Files) == 0 {
		return nil
	}
	if self.c.Modes().Review.FileIndex+1 < len(session.Files) {
		self.c.Modes().Review.FileIndex++
		self.c.Contexts().Review.GetList().SetSelection(self.c.Modes().Review.FileIndex)
		self.c.Modes().Review.HunkIndex = 0
		self.c.Modes().Review.DiffVerticalOffset = 0
	}
	self.renderReview()
	return nil
}

func (self *GlobalController) reviewPrevFile() error {
	if self.c.Modes().Review.FileIndex > 0 {
		self.c.Modes().Review.FileIndex--
		self.c.Contexts().Review.GetList().SetSelection(self.c.Modes().Review.FileIndex)
		self.c.Modes().Review.HunkIndex = 0
		self.c.Modes().Review.DiffVerticalOffset = 0
	}
	self.renderReview()
	return nil
}

func (self *GlobalController) reviewAcceptHunk() error {
	session := self.c.Model().ReviewSession
	if session == nil || len(session.Files) == 0 {
		return nil
	}
	fileIndex := clamp(self.c.Modes().Review.FileIndex, 0, len(session.Files)-1)
	hunks := reviewcore.Hunks(session.Files[fileIndex].Reviewed, session.Files[fileIndex].Current)
	if len(hunks) == 0 {
		self.c.Modes().Review.Message = "file is already caught up"
		self.renderReview()
		return nil
	}
	hunkIndex := clamp(self.c.Modes().Review.HunkIndex, 0, len(hunks)-1)
	next := reviewcore.ApplyHunk(session.Files[fileIndex].Reviewed, hunks[hunkIndex])
	outcome, err := session.AcceptFileContentLocal(fileIndex, next)
	if err != nil {
		self.c.ErrorToast(err.Error())
		return nil
	}
	remaining := len(reviewcore.Hunks(session.Files[fileIndex].Reviewed, session.Files[fileIndex].Current))
	if remaining == 0 {
		self.c.Modes().Review.HunkIndex = 0
		self.c.Modes().Review.Message = "accepted hunk, file caught up"
	} else {
		self.c.Modes().Review.HunkIndex = clamp(self.c.Modes().Review.HunkIndex, 0, remaining-1)
		self.c.Modes().Review.Message = fmt.Sprintf("accepted hunk, %d left", remaining)
	}
	self.c.Modes().Review.DiffVerticalOffset = 0
	self.appendReviewLog("accepted hunk in " + outcome.Path)
	self.markViewedIfNeeded(outcome)
	self.renderReview()
	return nil
}

func (self *GlobalController) reviewAcceptFile() error {
	session := self.c.Model().ReviewSession
	if session == nil || len(session.Files) == 0 {
		return nil
	}
	fileIndex := clamp(self.c.Modes().Review.FileIndex, 0, len(session.Files)-1)
	current := session.Files[fileIndex].Current
	outcome, err := session.AcceptFileContentLocal(fileIndex, current)
	if err != nil {
		self.c.ErrorToast(err.Error())
		return nil
	}
	self.c.Modes().Review.HunkIndex = 0
	self.c.Modes().Review.DiffVerticalOffset = 0
	self.c.Modes().Review.Message = "accepted file"
	self.appendReviewLog("accepted file " + outcome.Path)
	self.markViewedIfNeeded(outcome)
	self.renderReview()
	return nil
}

func (self *GlobalController) reviewToggleWrap() error {
	self.c.Modes().Review.ToggleDiffMode()
	self.c.GetAppState().Review.DiffViewMode = string(self.c.Modes().Review.DiffViewMode)
	self.c.GetAppState().Review.DiffHorizontalOffset = self.c.Modes().Review.DiffHorizontalOffset
	self.c.SaveAppStateAndLogError()
	self.c.Modes().Review.Message = "diff mode: " + string(self.c.Modes().Review.DiffViewMode)
	self.renderReview()
	return nil
}

func (self *GlobalController) reviewScrollDown() error {
	self.c.Modes().Review.DiffVerticalOffset += 3
	self.renderReview()
	return nil
}

func (self *GlobalController) reviewScrollUp() error {
	self.c.Modes().Review.DiffVerticalOffset = max(0, self.c.Modes().Review.DiffVerticalOffset-3)
	self.renderReview()
	return nil
}

func (self *GlobalController) reviewScrollLeft() error {
	if self.c.Modes().Review.DiffViewMode == reviewmode.DiffViewModeScroll {
		self.c.Modes().Review.DiffHorizontalOffset = max(0, self.c.Modes().Review.DiffHorizontalOffset-4)
		self.c.GetAppState().Review.DiffHorizontalOffset = self.c.Modes().Review.DiffHorizontalOffset
		self.c.SaveAppStateAndLogError()
	} else {
		self.c.Modes().Review.DiffVerticalOffset = max(0, self.c.Modes().Review.DiffVerticalOffset-3)
	}
	self.renderReview()
	return nil
}

func (self *GlobalController) reviewScrollRight() error {
	if self.c.Modes().Review.DiffViewMode == reviewmode.DiffViewModeScroll {
		self.c.Modes().Review.DiffHorizontalOffset += 4
		self.c.GetAppState().Review.DiffHorizontalOffset = self.c.Modes().Review.DiffHorizontalOffset
		self.c.SaveAppStateAndLogError()
	} else {
		self.c.Modes().Review.DiffVerticalOffset += 3
	}
	self.renderReview()
	return nil
}

func (self *GlobalController) currentReviewHunks() []reviewcore.Hunk {
	session := self.c.Model().ReviewSession
	if session == nil || len(session.Files) == 0 {
		return nil
	}
	file := session.Files[clamp(self.c.Modes().Review.FileIndex, 0, len(session.Files)-1)]
	return reviewcore.Hunks(file.Reviewed, file.Current)
}

func (self *GlobalController) markViewedIfNeeded(outcome reviewcore.AcceptedFileOutcome) {
	if !outcome.ShouldMarkViewed || self.c.Model().ReviewSession == nil {
		return
	}
	prID := self.c.Model().ReviewSession.Manifest.PR.ID
	self.c.Modes().Review.PendingMarks++
	self.appendReviewLog("queued GitHub viewed mark for " + outcome.Path)
	self.c.OnWorker(func(gocui.Task) error {
		err := reviewcore.NewGh("").MarkFileViewed(prID, outcome.Path)
		self.c.OnUIThread(func() error {
			self.c.Modes().Review.PendingMarks = max(0, self.c.Modes().Review.PendingMarks-1)
			if err != nil {
				self.appendReviewLog("failed to mark viewed in GitHub for " + outcome.Path + ": " + err.Error())
			} else {
				self.appendReviewLog("marked viewed in GitHub: " + outcome.Path)
			}
			self.renderReview()
			return nil
		})
		return nil
	})
}

func (self *GlobalController) loadReview(prArg string) error {
	backend := reviewcore.NewGh("")
	pr, err := backend.ResolvePR(prArg)
	if err != nil {
		return err
	}
	root, err := reviewcore.DefaultStateRoot()
	if err != nil {
		return err
	}
	session, err := reviewcore.LoadSession(root, pr)
	if err != nil {
		return err
	}
	self.c.OnUIThreadContentOnly(func() error {
		self.appendReviewLog(fmt.Sprintf("resolved PR #%d with %d changed files", pr.Number, len(pr.Files)))
		return nil
	})
	if err := session.RefreshFiles(backend, func(message string) {
		self.c.OnUIThreadContentOnly(func() error {
			self.appendReviewLog(message)
			return nil
		})
	}); err != nil {
		return err
	}
	self.c.Model().ReviewSession = session
	self.c.Modes().Review.ResetSelection()
	self.c.Contexts().Review.GetList().SetSelection(0)
	return nil
}

func (self *GlobalController) renderReview() {
	self.c.Contexts().Review.HandleRender()
	self.c.Contexts().Review.HandleRenderToMain()
}

func reviewDiffContent(mode reviewmode.Review, session *reviewcore.Session) string {
	if mode.Loading && session == nil {
		return "Loading PR review...\n"
	}
	if session == nil {
		return "No review loaded. Press <alt+r> to exit review mode or retry.\n"
	}
	if len(session.Files) == 0 {
		return "No files in PR.\n"
	}
	file := session.Files[clamp(mode.FileIndex, 0, len(session.Files)-1)]
	hunks := reviewcore.Hunks(file.Reviewed, file.Current)
	var b strings.Builder
	b.WriteString(file.Meta.Path)
	b.WriteString(" ")
	b.WriteString(remainingHunksLabel(len(hunks)))
	b.WriteString("\n\n")
	if len(hunks) == 0 {
		b.WriteString("File caught up to latest PR content\n")
		return b.String()
	}
	hunk := hunks[clamp(mode.HunkIndex, 0, len(hunks)-1)]
	for _, line := range hunk.Lines {
		switch line.Kind {
		case reviewcore.DiffLineEqual:
			b.WriteString(" " + displayLineText(line.Text) + "\n")
		case reviewcore.DiffLineDelete:
			b.WriteString(style.FgRed.Sprint("-" + displayLineText(line.Text) + "\n"))
		case reviewcore.DiffLineInsert:
			b.WriteString(style.FgGreen.Sprint("+" + displayLineText(line.Text) + "\n"))
		}
	}
	return b.String()
}

func (self *GlobalController) reviewStatusContent() string {
	return reviewStatusContent(self.c.Modes().Review, self.c.Model().ReviewLogs)
}

func reviewStatusContent(mode reviewmode.Review, logs []string) string {
	lines := []string{
		"",
		fmt.Sprintf("mode=%s pendingMarks=%d", mode.DiffViewMode, mode.PendingMarks),
		mode.Message,
		"",
		"Log:",
	}
	lines = append(lines, logs...)
	return strings.Join(lines, "\n")
}

func (self *GlobalController) appendReviewLog(message string) {
	logs := append(self.c.Model().ReviewLogs, message)
	if len(logs) > 100 {
		logs = logs[len(logs)-100:]
	}
	self.c.Model().ReviewLogs = logs
}

func remainingHunksLabel(count int) string {
	if count == 1 {
		return "1 hunk remaining"
	}
	return fmt.Sprintf("%d hunks remaining", count)
}

func displayLineText(text string) string {
	return strings.TrimRight(text, "\r\n")
}

func clamp(value int, minValue int, maxValue int) int {
	if value < minValue {
		return minValue
	}
	if value > maxValue {
		return maxValue
	}
	return value
}

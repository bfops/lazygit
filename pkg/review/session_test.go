package review

import (
	"fmt"
	"os"
	"sync"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
)

type fakeBackend struct {
	mu           sync.Mutex
	files        map[string]string
	changedPaths map[string]bool
	compareErr   error
	ensureErr    error
	active       int
	maxActive    int
	baseFetches  int
	headFetches  int
	ensureCalls  int
	compareCalls int
	delay        time.Duration
	markViewed   []string
	requestOrder []string
}

func (f *fakeBackend) EnsurePRRefs(PrMeta) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.ensureCalls++
	return f.ensureErr
}

func (f *fakeBackend) ChangedFilesBetween(string, string, string, []PrFile) (map[string]bool, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.compareCalls++
	return f.changedPaths, f.compareErr
}

func (f *fakeBackend) FileAtRef(repo string, ref string, path string) (string, bool, error) {
	f.mu.Lock()
	f.active++
	f.maxActive = max(f.maxActive, f.active)
	if ref == "base" {
		f.baseFetches++
	} else if ref == "head" {
		f.headFetches++
	}
	f.requestOrder = append(f.requestOrder, ref+":"+path)
	f.mu.Unlock()

	if f.delay > 0 {
		time.Sleep(f.delay)
	}

	f.mu.Lock()
	f.active--
	content, ok := f.files[ref+":"+path]
	f.mu.Unlock()
	if !ok {
		return "", false, fmt.Errorf("missing file %s:%s", ref, path)
	}
	return content, false, nil
}

func (f *fakeBackend) MarkFileViewed(pullRequestID string, path string) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.markViewed = append(f.markViewed, path)
	return nil
}

func testPR(files []PrFile) PrMeta {
	return PrMeta{
		ID:          "PR_node",
		Number:      123,
		URL:         "https://github.com/acme/repo/pull/123",
		BaseRefName: "main",
		BaseRefOID:  "base",
		HeadRefName: "feature",
		HeadRefOID:  "head",
		Files:       files,
	}
}

func TestRefreshFilesLoadsInParallelAndPreservesOrder(t *testing.T) {
	prFiles := make([]PrFile, 12)
	backendFiles := map[string]string{}
	for idx := range prFiles {
		path := fmt.Sprintf("file-%02d.txt", idx)
		prFiles[idx] = PrFile{Path: path, ChangeType: "MODIFIED"}
		backendFiles["base:"+path] = fmt.Sprintf("base %02d\n", idx)
		backendFiles["head:"+path] = fmt.Sprintf("head %02d\n", idx)
	}

	session := &Session{root: t.TempDir(), Manifest: Manifest{PR: testPR(prFiles)}}
	backend := &fakeBackend{files: backendFiles, delay: 10 * time.Millisecond}

	if err := session.RefreshFiles(backend, nil); err != nil {
		t.Fatal(err)
	}

	if backend.maxActive <= 1 {
		t.Fatalf("expected parallel file loading, max active requests was %d", backend.maxActive)
	}
	if backend.maxActive > 8 {
		t.Fatalf("expected bounded parallelism of 8, max active requests was %d", backend.maxActive)
	}
	for idx, file := range session.Files {
		expectedPath := prFiles[idx].Path
		if file.Meta.Path != expectedPath {
			t.Fatalf("file order mismatch at %d: got %s, want %s", idx, file.Meta.Path, expectedPath)
		}
	}
}

func TestRefreshFilesSkipsBaseFetchForExistingReviewedFileWithMetadata(t *testing.T) {
	prFiles := []PrFile{{Path: "existing.txt", ChangeType: "MODIFIED"}}
	session := &Session{root: t.TempDir(), Manifest: Manifest{PR: testPR(prFiles)}}
	if err := writeReviewed(session.reviewedPath("existing.txt"), "reviewed\n"); err != nil {
		t.Fatal(err)
	}
	basePath := "existing.txt"
	baseRef := "base"
	initialHash := HashContent("base\n")
	session.Manifest.Files = []FileState{{
		Path:                "existing.txt",
		BasePathUsed:        &basePath,
		BaseRefOIDUsed:      &baseRef,
		InitialReviewedHash: &initialHash,
	}}
	backend := &fakeBackend{files: map[string]string{"head:existing.txt": "current\n"}}

	if err := session.RefreshFiles(backend, nil); err != nil {
		t.Fatal(err)
	}

	if backend.baseFetches != 0 {
		t.Fatalf("expected no base fetch, got %d", backend.baseFetches)
	}
	if backend.headFetches != 1 {
		t.Fatalf("expected one head fetch, got %d", backend.headFetches)
	}
	if got := *session.Files[0].Meta.InitialReviewedHash; got != initialHash {
		t.Fatalf("initial hash changed: got %s, want %s", got, initialHash)
	}
}

func TestRefreshFilesReadsUnchangedCurrentContentFromCache(t *testing.T) {
	prFiles := []PrFile{{Path: "existing.txt", ChangeType: "MODIFIED"}}
	session := &Session{root: t.TempDir(), Manifest: Manifest{
		PR:                testPR(prFiles),
		FetchedHeadRefOID: "head",
		Files:             []FileState{{Path: "existing.txt"}},
	}}
	assert.NoError(t, writeReviewed(session.reviewedPath("existing.txt"), "reviewed\n"))
	assert.NoError(t, writeReviewed(session.currentPath("existing.txt"), "cached current\n"))
	backend := &fakeBackend{files: map[string]string{"base:existing.txt": "base\n"}}

	assert.NoError(t, session.RefreshFiles(backend, nil))

	assert.Equal(t, "cached current\n", session.Files[0].Current)
	assert.Equal(t, 0, backend.headFetches)
	assert.Equal(t, 0, backend.compareCalls)
}

func TestRefreshFilesFetchesOnlyChangedCurrentContent(t *testing.T) {
	prFiles := []PrFile{
		{Path: "unchanged.txt", ChangeType: "MODIFIED"},
		{Path: "changed.txt", ChangeType: "MODIFIED"},
	}
	session := &Session{root: t.TempDir(), Manifest: Manifest{
		PR:                testPR(prFiles),
		FetchedHeadRefOID: "old-head",
		Files: []FileState{
			{Path: "unchanged.txt"},
			{Path: "changed.txt"},
		},
	}}
	assert.NoError(t, writeReviewed(session.reviewedPath("unchanged.txt"), "reviewed unchanged\n"))
	assert.NoError(t, writeReviewed(session.reviewedPath("changed.txt"), "reviewed changed\n"))
	assert.NoError(t, writeReviewed(session.currentPath("unchanged.txt"), "cached unchanged\n"))
	assert.NoError(t, writeReviewed(session.currentPath("changed.txt"), "stale changed\n"))
	backend := &fakeBackend{
		files: map[string]string{
			"base:unchanged.txt": "base unchanged\n",
			"base:changed.txt":   "base changed\n",
			"head:changed.txt":   "fresh changed\n",
		},
		changedPaths: map[string]bool{"changed.txt": true},
	}

	assert.NoError(t, session.RefreshFiles(backend, nil))

	assert.Equal(t, "cached unchanged\n", session.Files[0].Current)
	assert.Equal(t, "fresh changed\n", session.Files[1].Current)
	assert.Equal(t, 1, backend.headFetches)
	assert.Equal(t, 1, backend.compareCalls)
}

func TestRefreshFilesFetchesMissingCurrentCacheEntry(t *testing.T) {
	prFiles := []PrFile{{Path: "missing.txt", ChangeType: "MODIFIED"}}
	session := &Session{root: t.TempDir(), Manifest: Manifest{
		PR:                testPR(prFiles),
		FetchedHeadRefOID: "head",
		Files:             []FileState{{Path: "missing.txt"}},
	}}
	assert.NoError(t, writeReviewed(session.reviewedPath("missing.txt"), "reviewed\n"))
	backend := &fakeBackend{files: map[string]string{
		"base:missing.txt": "base\n",
		"head:missing.txt": "current\n",
	}}

	assert.NoError(t, session.RefreshFiles(backend, nil))

	assert.Equal(t, "current\n", session.Files[0].Current)
	assert.Equal(t, 1, backend.headFetches)
}

func TestRefreshFilesFallsBackToFullRefreshWhenCompareFails(t *testing.T) {
	prFiles := []PrFile{
		{Path: "one.txt", ChangeType: "MODIFIED"},
		{Path: "two.txt", ChangeType: "MODIFIED"},
	}
	session := &Session{root: t.TempDir(), Manifest: Manifest{
		PR:                testPR(prFiles),
		FetchedHeadRefOID: "old-head",
		Files: []FileState{
			{Path: "one.txt"},
			{Path: "two.txt"},
		},
	}}
	assert.NoError(t, writeReviewed(session.reviewedPath("one.txt"), "reviewed one\n"))
	assert.NoError(t, writeReviewed(session.reviewedPath("two.txt"), "reviewed two\n"))
	backend := &fakeBackend{
		files: map[string]string{
			"base:one.txt": "base one\n",
			"base:two.txt": "base two\n",
			"head:one.txt": "current one\n",
			"head:two.txt": "current two\n",
		},
		compareErr: fmt.Errorf("compare failed"),
	}
	progress := []string{}

	assert.NoError(t, session.RefreshFiles(backend, func(message string) {
		progress = append(progress, message)
	}))

	assert.Equal(t, 2, backend.headFetches)
	assert.Contains(t, progress[0], "warning: failed to compare")
}

func TestRefreshFilesPrunesCurrentCacheAndUpdatesFetchedHead(t *testing.T) {
	prFiles := []PrFile{
		{Path: "kept.txt", ChangeType: "MODIFIED"},
		{Path: "deleted.txt", ChangeType: "DELETED"},
	}
	session := &Session{root: t.TempDir(), Manifest: Manifest{PR: testPR(prFiles)}}
	assert.NoError(t, writeReviewed(session.currentPath("stale.txt"), "stale\n"))
	backend := &fakeBackend{files: map[string]string{
		"base:kept.txt":    "base\n",
		"base:deleted.txt": "deleted base\n",
		"head:kept.txt":    "current\n",
	}}

	assert.NoError(t, session.RefreshFiles(backend, nil))

	assert.Equal(t, "head", session.Manifest.FetchedHeadRefOID)
	_, err := os.Stat(session.currentPath("stale.txt"))
	assert.True(t, os.IsNotExist(err))
	cached, err := os.ReadFile(session.currentPath("kept.txt"))
	assert.NoError(t, err)
	assert.Equal(t, "current\n", string(cached))
	_, err = os.Stat(session.currentPath("deleted.txt"))
	assert.True(t, os.IsNotExist(err))
}

func TestParseChangedFilesIncludesBothRenamePaths(t *testing.T) {
	changed := parseChangedFiles("M\tmodified.txt\nR100\told.txt\tnew.txt\nD\tdeleted.txt\n")

	assert.True(t, changed["modified.txt"])
	assert.True(t, changed["old.txt"])
	assert.True(t, changed["new.txt"])
	assert.True(t, changed["deleted.txt"])
}

func TestRenamedFileInitializesReviewedContentFromPreviousBasePath(t *testing.T) {
	prFiles := []PrFile{{Path: "new.txt", PreviousPath: "old.txt", ChangeType: "RENAMED"}}
	session := &Session{root: t.TempDir(), Manifest: Manifest{PR: testPR(prFiles)}}
	backend := &fakeBackend{files: map[string]string{
		"base:old.txt": "old base\n",
		"head:new.txt": "new head\n",
	}}

	if err := session.RefreshFiles(backend, nil); err != nil {
		t.Fatal(err)
	}

	if got := session.Files[0].Reviewed; got != "old base\n" {
		t.Fatalf("reviewed content = %q, want previous path base content", got)
	}
	if got := *session.Files[0].Meta.BasePathUsed; got != "old.txt" {
		t.Fatalf("base path = %q, want old.txt", got)
	}
}

func TestRefreshFilesInitializesHunkProgressFromRemainingHunks(t *testing.T) {
	prFiles := []PrFile{{Path: "file.txt", ChangeType: "MODIFIED"}}
	session := &Session{root: t.TempDir(), Manifest: Manifest{PR: testPR(prFiles)}}
	backend := &fakeBackend{files: map[string]string{
		"base:file.txt": "old\n",
		"head:file.txt": "new\n",
	}}

	assert.NoError(t, session.RefreshFiles(backend, nil))

	assert.Equal(t, 0, session.Files[0].Meta.ReviewedHunkCount)
	assert.Equal(t, 1, session.Files[0].Meta.TotalHunkCount)
}

func TestAcceptHunkIncrementsReviewedHunkCount(t *testing.T) {
	reviewed := "old one\nsame 1\nsame 2\nsame 3\nsame 4\nsame 5\nsame 6\nsame 7\nold two\n"
	current := "new one\nsame 1\nsame 2\nsame 3\nsame 4\nsame 5\nsame 6\nsame 7\nnew two\n"
	hunks := Hunks(reviewed, current)
	session := &Session{
		root: t.TempDir(),
		Files: []ReviewFile{
			{
				Meta:     FileState{Path: "file.txt", TotalHunkCount: 2},
				Reviewed: reviewed,
				Current:  current,
			},
		},
	}

	outcome, err := session.AcceptHunkContentLocal(0, ApplyHunk(reviewed, hunks[0]))

	assert.NoError(t, err)
	assert.False(t, outcome.ShouldMarkViewed)
	assert.Equal(t, 1, session.Files[0].Meta.ReviewedHunkCount)
	assert.Equal(t, 2, session.Files[0].Meta.TotalHunkCount)
}

func TestAcceptFileCompletesHunkProgress(t *testing.T) {
	session := &Session{
		root: t.TempDir(),
		Files: []ReviewFile{
			{
				Meta:     FileState{Path: "file.txt", ReviewedHunkCount: 1, TotalHunkCount: 3},
				Reviewed: "old\n",
				Current:  "new\n",
			},
		},
	}

	outcome, err := session.AcceptFileContentLocal(0, "new\n")

	assert.NoError(t, err)
	assert.True(t, outcome.ShouldMarkViewed)
	assert.Equal(t, 3, session.Files[0].Meta.ReviewedHunkCount)
	assert.Equal(t, 3, session.Files[0].Meta.TotalHunkCount)
}

func TestRefreshFilesPreservesReviewedHunkCountAndUpdatesTotal(t *testing.T) {
	prFiles := []PrFile{{Path: "file.txt", ChangeType: "MODIFIED"}}
	session := &Session{root: t.TempDir(), Manifest: Manifest{
		PR: testPR(prFiles),
		Files: []FileState{
			{Path: "file.txt", ReviewedHunkCount: 2, TotalHunkCount: 4},
		},
	}}
	assert.NoError(t, writeReviewed(session.reviewedPath("file.txt"), "old\n"))
	backend := &fakeBackend{files: map[string]string{
		"base:file.txt": "old\n",
		"head:file.txt": "new\n",
	}}

	assert.NoError(t, session.RefreshFiles(backend, nil))

	assert.Equal(t, 2, session.Files[0].Meta.ReviewedHunkCount)
	assert.Equal(t, 3, session.Files[0].Meta.TotalHunkCount)
}

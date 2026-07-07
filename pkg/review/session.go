package review

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
)

type Session struct {
	root     string
	Manifest Manifest
	Files    []ReviewFile
}

func DefaultStateRoot() (string, error) {
	if dataHome := os.Getenv("XDG_DATA_HOME"); dataHome != "" {
		return filepath.Join(dataHome, "better-review"), nil
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(home, ".local", "share", "better-review"), nil
}

func LoadSession(root string, pr PrMeta) (*Session, error) {
	if root == "" {
		var err error
		root, err = DefaultStateRoot()
		if err != nil {
			return nil, err
		}
	}

	sessionRoot := sessionRoot(root, pr)
	if err := os.MkdirAll(filepath.Join(sessionRoot, "reviewed"), 0o755); err != nil {
		return nil, err
	}

	manifest := Manifest{PR: pr, Files: []FileState{}}
	manifestPath := filepath.Join(sessionRoot, "manifest.json")
	if bytes, err := os.ReadFile(manifestPath); err == nil {
		if err := json.Unmarshal(bytes, &manifest); err != nil {
			return nil, err
		}
		manifest.PR = pr
	} else if !os.IsNotExist(err) {
		return nil, err
	}

	return &Session{root: root, Manifest: manifest, Files: []ReviewFile{}}, nil
}

func (s *Session) RefreshFiles(backend Backend, progress func(string)) error {
	const concurrency = 8

	files := make([]ReviewFile, len(s.Manifest.PR.Files))
	existing := map[string]FileState{}
	for _, state := range s.Manifest.Files {
		existing[state.Path] = state
	}

	type result struct {
		index int
		file  ReviewFile
		err   error
	}

	jobs := make(chan int)
	results := make(chan result, len(s.Manifest.PR.Files))
	workerCount := min(concurrency, len(s.Manifest.PR.Files))
	var wg sync.WaitGroup

	for range workerCount {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for idx := range jobs {
				prFile := s.Manifest.PR.Files[idx]
				file, err := s.loadReviewFile(backend, prFile, existing[prFile.Path])
				results <- result{index: idx, file: file, err: err}
			}
		}()
	}

	go func() {
		for idx := range s.Manifest.PR.Files {
			jobs <- idx
		}
		close(jobs)
		wg.Wait()
		close(results)
	}()

	completed := 0
	var firstErr error
	for result := range results {
		completed++
		if progress != nil {
			progress(fmt.Sprintf("loaded file %d/%d: %s", completed, len(s.Manifest.PR.Files), s.Manifest.PR.Files[result.index].Path))
		}
		if result.err != nil && firstErr == nil {
			firstErr = result.err
			continue
		}
		files[result.index] = result.file
	}

	if firstErr != nil {
		return firstErr
	}

	return s.ApplyRefreshedFiles(files)
}

func (s *Session) ApplyRefreshedFiles(files []ReviewFile) error {
	s.Files = files
	s.Manifest.Files = make([]FileState, 0, len(files))
	for _, file := range files {
		s.Manifest.Files = append(s.Manifest.Files, file.Meta)
	}
	return s.Save()
}

func (s *Session) AcceptFileContentLocal(index int, content string) (AcceptedFileOutcome, error) {
	return s.acceptFileContentLocal(index, content, func(file *ReviewFile, _ int) {
		file.Meta.ReviewedHunkCount = file.Meta.TotalHunkCount
	})
}

func (s *Session) AcceptHunkContentLocal(index int, content string) (AcceptedFileOutcome, error) {
	return s.acceptFileContentLocal(index, content, func(file *ReviewFile, remaining int) {
		file.Meta.ReviewedHunkCount++
		file.Meta.TotalHunkCount = max(file.Meta.TotalHunkCount, file.Meta.ReviewedHunkCount+remaining)
		if remaining == 0 {
			file.Meta.ReviewedHunkCount = file.Meta.TotalHunkCount
		}
	})
}

func (s *Session) acceptFileContentLocal(index int, content string, updateProgress func(*ReviewFile, int)) (AcceptedFileOutcome, error) {
	path := s.Files[index].Meta.Path
	if err := writeReviewed(s.reviewedPath(path), content); err != nil {
		return AcceptedFileOutcome{}, err
	}

	s.Files[index].Reviewed = content
	s.Files[index].Meta.ReviewedHash = HashContent(content)
	remaining := len(Hunks(s.Files[index].Reviewed, s.Files[index].Current))
	updateProgress(&s.Files[index], remaining)
	shouldMarkViewed := s.Files[index].Reviewed == s.Files[index].Current
	s.Manifest.Files = make([]FileState, 0, len(s.Files))
	for _, file := range s.Files {
		s.Manifest.Files = append(s.Manifest.Files, file.Meta)
	}
	if err := s.Save(); err != nil {
		return AcceptedFileOutcome{}, err
	}
	return AcceptedFileOutcome{Path: path, ShouldMarkViewed: shouldMarkViewed}, nil
}

func (s *Session) Save() error {
	sessionRoot := sessionRoot(s.root, s.Manifest.PR)
	if err := os.MkdirAll(sessionRoot, 0o755); err != nil {
		return err
	}
	bytes, err := json.MarshalIndent(s.Manifest, "", "  ")
	if err != nil {
		return err
	}
	return os.WriteFile(filepath.Join(sessionRoot, "manifest.json"), bytes, 0o644)
}

func (s *Session) loadReviewFile(backend Backend, prFile PrFile, existing FileState) (ReviewFile, error) {
	reviewedPath := s.reviewedPath(prFile.Path)
	_, statErr := os.Stat(reviewedPath)
	needsInitial := os.IsNotExist(statErr)
	if statErr != nil && !os.IsNotExist(statErr) {
		return ReviewFile{}, statErr
	}

	var initial string
	if needsInitial {
		var err error
		initial, err = initialContent(backend, s.Manifest.PR, prFile)
		if err != nil {
			return ReviewFile{}, err
		}
		if err := writeReviewed(reviewedPath, initial); err != nil {
			return ReviewFile{}, err
		}
	}

	reviewedBytes, err := os.ReadFile(reviewedPath)
	if err != nil {
		return ReviewFile{}, err
	}
	current, err := currentContent(backend, s.Manifest.PR, prFile)
	if err != nil {
		return ReviewFile{}, err
	}

	reviewed := string(reviewedBytes)
	reviewed, initial, err = repairRenamedReviewedStateIfNeeded(backend, s.Manifest.PR, reviewedPath, prFile, reviewed, current, initial, existing)
	if err != nil {
		return ReviewFile{}, err
	}
	if initial == "" && shouldRecordInitialMetadata(s.Manifest.PR, prFile, existing) {
		initial, err = initialContent(backend, s.Manifest.PR, prFile)
		if err != nil {
			return ReviewFile{}, err
		}
	}

	currentHash := HashContent(current)
	basePath := initialBasePath(prFile)
	baseRefOID := s.Manifest.PR.BaseRefOID
	initialReviewedHash := HashContent(initial)
	if initial == "" && existing.InitialReviewedHash != nil {
		initialReviewedHash = *existing.InitialReviewedHash
	}
	remainingHunks := len(Hunks(reviewed, current))
	reviewedHunkCount := existing.ReviewedHunkCount
	totalHunkCount := reviewedHunkCount + remainingHunks
	previousPath := optionalString(prFile.PreviousPath)
	return ReviewFile{
		Meta: FileState{
			Path:                prFile.Path,
			PreviousPath:        previousPath,
			ChangeType:          prFile.ChangeType,
			ReviewedHash:        HashContent(reviewed),
			CurrentHash:         &currentHash,
			BasePathUsed:        &basePath,
			BaseRefOIDUsed:      &baseRefOID,
			InitialReviewedHash: &initialReviewedHash,
			ReviewedHunkCount:   reviewedHunkCount,
			TotalHunkCount:      totalHunkCount,
		},
		Reviewed: reviewed,
		Current:  current,
	}, nil
}

func repairRenamedReviewedStateIfNeeded(backend Backend, pr PrMeta, path string, file PrFile, reviewed string, current string, initial string, existing FileState) (string, string, error) {
	if !isRenamed(file) || file.PreviousPath == "" {
		return reviewed, initial, nil
	}

	if reviewed == "" && current != "" && initial == "" {
		var err error
		initial, err = initialContent(backend, pr, file)
		if err != nil {
			return "", "", err
		}
	}
	if reviewed == "" && current != "" && initial != "" {
		// TODO(remove after pre-rename-fix state is obsolete): repair reviewed files
		// generated as empty additions before renamed files loaded previous_path content.
		if err := writeReviewed(path, initial); err != nil {
			return "", "", err
		}
		return initial, initial, nil
	}

	expectedBasePath := initialBasePath(file)
	wasInitializedFromWrongPath := existing.BasePathUsed != nil && *existing.BasePathUsed == file.Path && *existing.BasePathUsed != expectedBasePath
	isUnmodifiedGeneratedState := existing.InitialReviewedHash != nil && *existing.InitialReviewedHash == HashContent(reviewed)
	if wasInitializedFromWrongPath && isUnmodifiedGeneratedState {
		var err error
		if initial == "" {
			initial, err = initialContent(backend, pr, file)
			if err != nil {
				return "", "", err
			}
		}
		// TODO(remove after pre-rename-fix state is obsolete): repair reviewed files
		// generated before renamed files were initialized from previous_path.
		if err := writeReviewed(path, initial); err != nil {
			return "", "", err
		}
		return initial, initial, nil
	}

	return reviewed, initial, nil
}

func shouldRecordInitialMetadata(pr PrMeta, file PrFile, existing FileState) bool {
	if existing.InitialReviewedHash == nil || existing.BasePathUsed == nil || existing.BaseRefOIDUsed == nil {
		return true
	}
	return *existing.BasePathUsed != initialBasePath(file) || *existing.BaseRefOIDUsed != pr.BaseRefOID
}

func initialContent(backend Backend, pr PrMeta, file PrFile) (string, error) {
	if strings.EqualFold(file.ChangeType, "ADDED") {
		return "", nil
	}
	content, _, err := backend.FileAtRef(baseRepoName(pr), pr.BaseRefOID, initialBasePath(file))
	return content, err
}

func currentContent(backend Backend, pr PrMeta, file PrFile) (string, error) {
	if strings.EqualFold(file.ChangeType, "DELETED") {
		return "", nil
	}
	content, _, err := backend.FileAtRef(headRepoName(pr), pr.HeadRefOID, file.Path)
	return content, err
}

func initialBasePath(file PrFile) string {
	if file.PreviousPath != "" {
		return file.PreviousPath
	}
	return file.Path
}

func isRenamed(file PrFile) bool {
	return strings.EqualFold(file.ChangeType, "RENAMED") || strings.EqualFold(file.ChangeType, "MOVED")
}

func headRepoName(pr PrMeta) string {
	if pr.HeadRepository != nil && pr.HeadRepository.NameWithOwner != "" {
		return pr.HeadRepository.NameWithOwner
	}
	return baseRepoName(pr)
}

func baseRepoName(pr PrMeta) string {
	const marker = "github.com/"
	idx := strings.Index(pr.URL, marker)
	if idx < 0 {
		return "unknown/unknown"
	}
	rest := pr.URL[idx+len(marker):]
	parts := strings.Split(rest, "/")
	if len(parts) < 2 {
		return "unknown/unknown"
	}
	return parts[0] + "/" + parts[1]
}

func sessionRoot(root string, pr PrMeta) string {
	repo := baseRepoName(pr)
	parts := strings.Split(repo, "/")
	owner, name := "unknown", "unknown"
	if len(parts) >= 2 {
		owner, name = parts[0], parts[1]
	}
	return filepath.Join(root, "github.com", owner, name, fmt.Sprintf("pr-%d", pr.Number))
}

func (s *Session) reviewedPath(path string) string {
	return filepath.Join(sessionRoot(s.root, s.Manifest.PR), "reviewed", filepath.FromSlash(path))
}

func writeReviewed(path string, content string) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	return os.WriteFile(path, []byte(content), 0o644)
}

func optionalString(value string) *string {
	if value == "" {
		return nil
	}
	return &value
}

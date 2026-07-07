package review

import (
	"encoding/base64"
	"encoding/json"
	"fmt"
	"os/exec"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
)

type Gh struct {
	repo string
}

func NewGh(repo string) *Gh {
	return &Gh{repo: repo}
}

func (g *Gh) ResolvePR(pr string) (PrMeta, error) {
	args := []string{}
	if g.repo != "" {
		args = append(args, "-R", g.repo)
	}
	args = append(args, "pr", "view")
	if pr != "" {
		args = append(args, pr)
	}
	args = append(args, "--json", "id,number,url,baseRefName,baseRefOid,headRefName,headRefOid,files,headRepository")

	output, err := runGh(args...)
	if err != nil {
		return PrMeta{}, err
	}

	var response struct {
		ID          string `json:"id"`
		Number      int    `json:"number"`
		URL         string `json:"url"`
		BaseRefName string `json:"baseRefName"`
		BaseRefOID  string `json:"baseRefOid"`
		HeadRefName string `json:"headRefName"`
		HeadRefOID  string `json:"headRefOid"`
		Files       []struct {
			Path         string `json:"path"`
			PreviousPath string `json:"previousPath"`
			Additions    int    `json:"additions"`
			Deletions    int    `json:"deletions"`
			ChangeType   string `json:"changeType"`
		} `json:"files"`
		HeadRepository *struct {
			NameWithOwner string `json:"nameWithOwner"`
		} `json:"headRepository"`
	}
	if err := json.Unmarshal([]byte(output), &response); err != nil {
		return PrMeta{}, fmt.Errorf("failed to parse gh pr view JSON: %w", err)
	}

	files := make([]PrFile, 0, len(response.Files))
	for _, file := range response.Files {
		files = append(files, PrFile{
			Path:         file.Path,
			PreviousPath: file.PreviousPath,
			Additions:    file.Additions,
			Deletions:    file.Deletions,
			ChangeType:   file.ChangeType,
		})
	}

	var headRepository *RepoRef
	if response.HeadRepository != nil {
		headRepository = &RepoRef{NameWithOwner: response.HeadRepository.NameWithOwner}
	}
	meta := PrMeta{
		ID:             response.ID,
		Number:         response.Number,
		URL:            response.URL,
		BaseRefName:    response.BaseRefName,
		BaseRefOID:     response.BaseRefOID,
		HeadRefName:    response.HeadRefName,
		HeadRefOID:     response.HeadRefOID,
		Files:          files,
		HeadRepository: headRepository,
	}

	if err := g.enrichRenamedFiles(&meta, pr); err != nil {
		return PrMeta{}, err
	}
	return meta, nil
}

func (g *Gh) EnsurePRRefs(pr PrMeta) error {
	missingBase := !gitObjectExists(pr.BaseRefOID)
	missingHead := !gitObjectExists(pr.HeadRefOID)
	if !missingBase && !missingHead {
		return nil
	}

	repoURL := gitRepoURL(pr)
	if missingHead {
		if _, err := runGit("fetch", "--no-tags", repoURL, fmt.Sprintf("refs/pull/%d/head", pr.Number)); err != nil {
			return err
		}
	}
	if missingBase && pr.BaseRefName != "" {
		if _, err := runGit("fetch", "--no-tags", repoURL, "refs/heads/"+pr.BaseRefName); err != nil {
			return err
		}
	}

	missing := []string{}
	if !gitObjectExists(pr.BaseRefOID) {
		missing = append(missing, "base "+pr.BaseRefOID)
	}
	if !gitObjectExists(pr.HeadRefOID) {
		missing = append(missing, "head "+pr.HeadRefOID)
	}
	if len(missing) > 0 {
		return fmt.Errorf("missing git objects after fetch: %s", strings.Join(missing, ", "))
	}
	return nil
}

func (g *Gh) ChangedFilesBetween(_ string, fromRef string, toRef string, files []PrFile) (map[string]bool, error) {
	args := []string{"diff", "--name-status", "-M", fromRef, toRef, "--"}
	for _, file := range files {
		args = append(args, file.Path)
		if file.PreviousPath != "" {
			args = append(args, file.PreviousPath)
		}
	}
	output, err := runGit(args...)
	if err != nil {
		return nil, err
	}
	return parseChangedFiles(output), nil
}

func (g *Gh) FileAtRef(repo string, ref string, path string) (string, bool, error) {
	output, err := runGit("show", ref+":"+path)
	if err != nil {
		return "", false, err
	}
	return output, false, nil
}

func (g *Gh) MarkFileViewed(pullRequestID string, path string) error {
	query := `
mutation($input: MarkFileAsViewedInput!) {
  markFileAsViewed(input: $input) {
    pullRequest {
      id
    }
  }
}
`
	inputBytes, err := json.Marshal(map[string]string{
		"pullRequestId": pullRequestID,
		"path":          path,
	})
	if err != nil {
		return err
	}
	args := []string{"api", "graphql", "-f", "query=" + query, "-F", "input=" + string(inputBytes)}
	_, err = runGh(args...)
	return err
}

func (g *Gh) enrichRenamedFiles(meta *PrMeta, pr string) error {
	renameMap, err := g.renameMap(pr)
	if err != nil {
		return err
	}
	for idx := range meta.Files {
		file := &meta.Files[idx]
		if !isRenamed(*file) || file.PreviousPath != "" {
			continue
		}
		previous, ok := renameMap[file.Path]
		if !ok {
			return fmt.Errorf("renamed file %s was missing from patch rename headers", file.Path)
		}
		file.PreviousPath = previous
	}
	return nil
}

func (g *Gh) renameMap(pr string) (map[string]string, error) {
	args := []string{}
	if g.repo != "" {
		args = append(args, "-R", g.repo)
	}
	args = append(args, "pr", "diff", "--patch")
	if pr != "" {
		args = append(args, pr)
	}
	output, err := runGh(args...)
	if err != nil {
		return nil, err
	}
	return parseRenamePathsFromPatch(output), nil
}

func parseRenamePathsFromPatch(patch string) map[string]string {
	result := map[string]string{}
	var oldPath string
	for _, line := range strings.Split(patch, "\n") {
		switch {
		case strings.HasPrefix(line, "rename from "):
			oldPath = strings.TrimPrefix(line, "rename from ")
		case strings.HasPrefix(line, "rename to ") && oldPath != "":
			result[strings.TrimPrefix(line, "rename to ")] = oldPath
			oldPath = ""
		case strings.HasPrefix(line, "diff --git "):
			oldPath = ""
		}
	}
	return result
}

func runGh(args ...string) (string, error) {
	cmd := exec.Command("gh", args...)
	output, err := cmd.CombinedOutput()
	if err != nil {
		return "", fmt.Errorf("command %q failed: %w\n%s", append([]string{"gh"}, args...), err, string(output))
	}
	return string(output), nil
}

func runGit(args ...string) (string, error) {
	cmd := exec.Command("git", args...)
	output, err := cmd.CombinedOutput()
	if err != nil {
		return "", fmt.Errorf("command %q failed: %w\n%s", append([]string{"git"}, args...), err, string(output))
	}
	return string(output), nil
}

func gitObjectExists(ref string) bool {
	if ref == "" {
		return false
	}
	_, err := runGit("cat-file", "-e", ref+"^{object}")
	return err == nil
}

func parseChangedFiles(output string) map[string]bool {
	result := map[string]bool{}
	for _, line := range strings.Split(strings.TrimSpace(output), "\n") {
		if line == "" {
			continue
		}
		parts := strings.Split(line, "\t")
		if len(parts) < 2 {
			continue
		}
		if strings.HasPrefix(parts[0], "R") || strings.HasPrefix(parts[0], "C") {
			if len(parts) >= 3 {
				result[parts[1]] = true
				result[parts[2]] = true
			}
			continue
		}
		result[parts[1]] = true
	}
	return result
}

func gitRepoURL(pr PrMeta) string {
	repo := baseRepoName(pr)
	if strings.HasPrefix(pr.URL, "http://") {
		return "http://" + filepath.ToSlash(filepath.Join("github.com", repo)) + ".git"
	}
	return "https://" + filepath.ToSlash(filepath.Join("github.com", repo)) + ".git"
}

func splitRepo(repo string) (owner string, name string, host string) {
	host = "github.com"
	repo = strings.TrimPrefix(repo, "https://")
	repo = strings.TrimPrefix(repo, "http://")
	repo = strings.TrimSuffix(repo, ".git")
	if strings.Contains(repo, "/") {
		parts := strings.Split(repo, "/")
		if len(parts) >= 3 && strings.Contains(parts[0], ".") {
			host = parts[0]
			return parts[1], parts[2], host
		}
		return parts[0], parts[1], host
	}
	return "unknown", "unknown", host
}

var nodeIDRegexp = regexp.MustCompile(`^[A-Za-z0-9+/]+={0,2}$`)

func DecodeNodeIDForDebug(nodeID string) string {
	if !nodeIDRegexp.MatchString(nodeID) {
		return ""
	}
	decoded, err := base64.StdEncoding.DecodeString(nodeID)
	if err != nil {
		return ""
	}
	return string(decoded)
}

func PRNumberString(pr PrMeta) string {
	return strconv.Itoa(pr.Number)
}

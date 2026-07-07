package review

import (
	"encoding/base64"
	"encoding/json"
	"fmt"
	"os/exec"
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
	args = append(args, "--json", "id,number,url,baseRefOid,headRefOid,files,headRepository")

	output, err := runGh(args...)
	if err != nil {
		return PrMeta{}, err
	}

	var response struct {
		ID         string `json:"id"`
		Number     int    `json:"number"`
		URL        string `json:"url"`
		BaseRefOID string `json:"baseRefOid"`
		HeadRefOID string `json:"headRefOid"`
		Files      []struct {
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
		BaseRefOID:     response.BaseRefOID,
		HeadRefOID:     response.HeadRefOID,
		Files:          files,
		HeadRepository: headRepository,
	}

	if err := g.enrichRenamedFiles(&meta, pr); err != nil {
		return PrMeta{}, err
	}
	return meta, nil
}

func (g *Gh) FileAtRef(repo string, ref string, path string) (string, bool, error) {
	owner, name, host := splitRepo(repo)
	query := `
query($owner: String!, $name: String!, $expr: String!) {
  repository(owner: $owner, name: $name) {
    object(expression: $expr) {
      ... on Blob {
        isBinary
        text
        byteSize
      }
    }
  }
}
`
	args := []string{"api", "graphql", "-f", "query=" + query, "-F", "owner=" + owner, "-F", "name=" + name, "-F", "expr=" + ref + ":" + path}
	if host != "" && host != "github.com" {
		args = append(args, "--hostname", host)
	}
	output, err := runGh(args...)
	if err != nil {
		return "", false, err
	}

	var response struct {
		Data struct {
			Repository struct {
				Object *struct {
					IsBinary bool    `json:"isBinary"`
					Text     *string `json:"text"`
					ByteSize int     `json:"byteSize"`
				} `json:"object"`
			} `json:"repository"`
		} `json:"data"`
	}
	if err := json.Unmarshal([]byte(output), &response); err != nil {
		return "", false, err
	}
	if response.Data.Repository.Object == nil {
		return "", false, nil
	}
	if response.Data.Repository.Object.IsBinary {
		return "", true, nil
	}
	if response.Data.Repository.Object.Text == nil {
		return "", false, nil
	}
	return *response.Data.Repository.Object.Text, false, nil
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

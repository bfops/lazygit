package review

type PrMeta struct {
	ID             string   `json:"id"`
	Number         int      `json:"number"`
	URL            string   `json:"url"`
	BaseRefOID     string   `json:"base_ref_oid"`
	HeadRefOID     string   `json:"head_ref_oid"`
	Files          []PrFile `json:"files"`
	HeadRepository *RepoRef `json:"head_repository"`
}

type RepoRef struct {
	NameWithOwner string `json:"name_with_owner"`
}

type PrFile struct {
	Path         string `json:"path"`
	PreviousPath string `json:"previous_path"`
	Additions    int    `json:"additions"`
	Deletions    int    `json:"deletions"`
	ChangeType   string `json:"change_type"`
}

type Manifest struct {
	PR    PrMeta      `json:"pr"`
	Files []FileState `json:"files"`
}

type FileState struct {
	Path                string  `json:"path"`
	PreviousPath        *string `json:"previous_path,omitempty"`
	ChangeType          string  `json:"change_type"`
	ReviewedHash        string  `json:"reviewed_hash"`
	CurrentHash         *string `json:"current_hash,omitempty"`
	BasePathUsed        *string `json:"base_path_used,omitempty"`
	BaseRefOIDUsed      *string `json:"base_ref_oid_used,omitempty"`
	InitialReviewedHash *string `json:"initial_reviewed_hash,omitempty"`
	Unsupported         bool    `json:"unsupported"`
}

type ReviewFile struct {
	Meta     FileState
	Reviewed string
	Current  string
}

type AcceptedFileOutcome struct {
	Path             string
	ShouldMarkViewed bool
}

type DiffLineKind int

const (
	DiffLineEqual DiffLineKind = iota
	DiffLineDelete
	DiffLineInsert
)

type DiffLine struct {
	Kind DiffLineKind
	Text string
}

type Hunk struct {
	Lines []DiffLine
}

type Backend interface {
	FileAtRef(repo string, ref string, path string) (string, bool, error)
	MarkFileViewed(pullRequestID string, path string) error
}

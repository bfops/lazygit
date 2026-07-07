use std::collections::HashMap;
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use anyhow::{anyhow, Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::github::{hash_content, Gh, PrFile, PrMeta};
use crate::log_buffer::LogBuffer;

const DEFAULT_FETCH_CONCURRENCY: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub pr: PrMeta,
    pub files: Vec<FileState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileState {
    pub path: String,
    pub previous_path: Option<String>,
    pub change_type: String,
    pub reviewed_hash: String,
    pub current_hash: Option<String>,
    pub base_path_used: Option<String>,
    pub base_ref_oid_used: Option<String>,
    pub initial_reviewed_hash: Option<String>,
    pub unsupported: bool,
}

#[derive(Debug, Clone)]
pub struct ReviewFile {
    pub meta: FileState,
    pub reviewed: String,
    pub current: String,
}

#[derive(Debug, Clone)]
pub struct ReviewSession {
    root: PathBuf,
    pub manifest: Manifest,
    pub files: Vec<ReviewFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedFileOutcome {
    pub path: String,
    pub should_mark_viewed: bool,
}

pub fn state_root(override_path: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = override_path {
        return Ok(path);
    }
    let dirs = ProjectDirs::from("dev", "better-review", "better-review")
        .context("could not find platform data directory")?;
    Ok(dirs.data_dir().to_path_buf())
}

impl ReviewSession {
    pub fn load(root: PathBuf, pr: PrMeta) -> Result<Self> {
        let session_root = session_root(&root, &pr);
        fs::create_dir_all(session_root.join("reviewed"))?;
        let manifest_path = session_root.join("manifest.json");
        let manifest = if manifest_path.exists() {
            let text = fs::read_to_string(&manifest_path)?;
            let mut manifest: Manifest = serde_json::from_str(&text)?;
            manifest.pr = pr;
            manifest
        } else {
            Manifest {
                pr,
                files: Vec::new(),
            }
        };
        Ok(Self {
            root,
            manifest,
            files: Vec::new(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn refresh_files_with_logs(&mut self, gh: &Gh, logs: Option<&LogBuffer>) -> Result<()> {
        let files = load_review_files_with_existing(
            &self.root,
            &self.manifest.pr,
            gh,
            &self.manifest.files,
            |message| {
                if let Some(logs) = logs {
                    logs.record(message);
                }
            },
        )?;
        self.apply_refreshed_files(files)
    }

    pub fn apply_refreshed_files(&mut self, files: Vec<ReviewFile>) -> Result<()> {
        self.manifest.files = files.iter().map(|file| file.meta.clone()).collect();
        self.files = files;
        self.save()
    }

    pub fn accept_file_content_local(
        &mut self,
        index: usize,
        content: String,
    ) -> Result<AcceptedFileOutcome> {
        let path = self.files[index].meta.path.clone();
        let reviewed_path = self.reviewed_path(&path);
        write_reviewed(&reviewed_path, &content)?;
        self.files[index].reviewed = content;
        self.files[index].meta.reviewed_hash = hash_content(&self.files[index].reviewed);
        let should_mark_viewed = self.files[index].reviewed == self.files[index].current;
        self.manifest.files = self.files.iter().map(|file| file.meta.clone()).collect();
        self.save()?;
        Ok(AcceptedFileOutcome {
            path,
            should_mark_viewed,
        })
    }

    fn save(&self) -> Result<()> {
        let session_root = session_root(&self.root, &self.manifest.pr);
        fs::create_dir_all(&session_root)?;
        let text = serde_json::to_string_pretty(&self.manifest)?;
        fs::write(session_root.join("manifest.json"), text)?;
        Ok(())
    }

    fn reviewed_path(&self, path: &str) -> PathBuf {
        reviewed_path(&self.root, &self.manifest.pr, path)
    }
}

pub fn load_review_files(
    root: &Path,
    pr: &PrMeta,
    gh: &Gh,
    progress: impl FnMut(String),
) -> Result<Vec<ReviewFile>> {
    load_review_files_with_existing(root, pr, gh, &[], progress)
}

pub fn load_review_files_with_existing(
    root: &Path,
    pr: &PrMeta,
    gh: &Gh,
    existing_states: &[FileState],
    mut progress: impl FnMut(String),
) -> Result<Vec<ReviewFile>> {
    let files = pr.files.clone();
    if files.is_empty() {
        return Ok(Vec::new());
    }

    let worker_count = files.len().min(DEFAULT_FETCH_CONCURRENCY);
    progress(format!(
        "loading {} files with {} workers",
        files.len(),
        worker_count
    ));

    let existing_by_path: Arc<HashMap<String, FileState>> = Arc::new(
        existing_states
            .iter()
            .map(|state| (state.path.clone(), state.clone()))
            .collect(),
    );
    let queue = Arc::new(Mutex::new(
        files.into_iter().enumerate().collect::<VecDeque<_>>(),
    ));
    let (tx, rx) = mpsc::channel();
    let total = pr.files.len();

    thread::scope(|scope| {
        for _ in 0..worker_count {
            let queue = Arc::clone(&queue);
            let existing_by_path = Arc::clone(&existing_by_path);
            let tx = tx.clone();
            scope.spawn(move || loop {
                let Some((idx, pr_file)) = queue.lock().expect("file queue poisoned").pop_front()
                else {
                    break;
                };
                let _ = tx.send(LoadMessage::Progress(format!(
                    "loading file {}/{}: {}",
                    idx + 1,
                    total,
                    pr_file.path
                )));
                let existing_state = existing_by_path.get(&pr_file.path);
                let result = load_review_file(root, pr, gh, pr_file, existing_state)
                    .map_err(|error| error.to_string());
                let _ = tx.send(LoadMessage::Loaded { idx, result });
            });
        }
        drop(tx);

        let mut loaded = vec![None; total];
        let mut first_error = None;
        for message in rx {
            match message {
                LoadMessage::Progress(message) => progress(message),
                LoadMessage::Loaded { idx, result } => match result {
                    Ok(file) => loaded[idx] = Some(file),
                    Err(error) => {
                        if first_error.is_none() {
                            first_error = Some(error);
                        }
                    }
                },
            }
        }

        if let Some(error) = first_error {
            return Err(anyhow!(error));
        }

        loaded
            .into_iter()
            .enumerate()
            .map(|(idx, file)| {
                file.ok_or_else(|| anyhow!("worker did not return file at index {idx}"))
            })
            .collect()
    })
}

enum LoadMessage {
    Progress(String),
    Loaded {
        idx: usize,
        result: Result<ReviewFile, String>,
    },
}

fn load_review_file(
    root: &Path,
    pr: &PrMeta,
    gh: &Gh,
    pr_file: PrFile,
    existing_state: Option<&FileState>,
) -> Result<ReviewFile> {
    let reviewed_path = reviewed_path(root, pr, &pr_file.path);
    let base_path = initial_base_path(&pr_file);
    let initial = initial_content(gh, pr, &pr_file)?;
    if !reviewed_path.exists() {
        write_reviewed(&reviewed_path, &initial)?;
    }

    let reviewed = fs::read_to_string(&reviewed_path)
        .with_context(|| format!("failed to read {}", reviewed_path.display()))?;
    let current = current_content(gh, pr, &pr_file)?.unwrap_or_default();
    let reviewed = repair_renamed_reviewed_state_if_needed(
        &reviewed_path,
        &pr_file,
        reviewed,
        &current,
        &initial,
        existing_state,
    )?;
    let unsupported = false;
    Ok(ReviewFile {
        meta: FileState {
            path: pr_file.path,
            previous_path: pr_file.previous_path,
            change_type: pr_file.change_type,
            reviewed_hash: hash_content(&reviewed),
            current_hash: Some(hash_content(&current)),
            base_path_used: Some(base_path),
            base_ref_oid_used: Some(pr.base_ref_oid.clone()),
            initial_reviewed_hash: Some(hash_content(&initial)),
            unsupported,
        },
        reviewed,
        current,
    })
}

fn repair_renamed_reviewed_state_if_needed(
    reviewed_path: &Path,
    file: &PrFile,
    reviewed: String,
    current: &str,
    initial: &str,
    existing_state: Option<&FileState>,
) -> Result<String> {
    if !is_renamed(file) || file.previous_path.is_none() {
        return Ok(reviewed);
    }

    if reviewed.is_empty() && !current.is_empty() && !initial.is_empty() {
        // TODO(remove after pre-rename-fix state is obsolete): repair reviewed files
        // generated as empty additions before renamed files loaded previous_path content.
        write_reviewed(reviewed_path, initial)?;
        return Ok(initial.to_owned());
    }

    let expected_base_path = initial_base_path(file);
    if let Some(existing_state) = existing_state {
        let was_initialized_from_wrong_path = existing_state.base_path_used.as_deref()
            == Some(file.path.as_str())
            && existing_state.base_path_used.as_deref() != Some(expected_base_path.as_str());
        let is_unmodified_generated_state = existing_state
            .initial_reviewed_hash
            .as_deref()
            .map(|hash| hash == hash_content(&reviewed))
            .unwrap_or(false);

        if was_initialized_from_wrong_path && is_unmodified_generated_state {
            // TODO(remove after pre-rename-fix state is obsolete): repair reviewed files
            // generated before renamed files were initialized from previous_path.
            write_reviewed(reviewed_path, initial)?;
            return Ok(initial.to_owned());
        }
        return Ok(reviewed);
    }

    Ok(reviewed)
}

fn initial_content(gh: &Gh, pr: &PrMeta, file: &PrFile) -> Result<String> {
    if file.change_type.eq_ignore_ascii_case("ADDED") {
        return Ok(String::new());
    }
    let base_path = initial_base_path(file);
    Ok(gh
        .file_at_ref(
            base_repo_name(pr).as_deref().unwrap_or("unknown/unknown"),
            &pr.base_ref_oid,
            &base_path,
        )?
        .unwrap_or_default())
}

fn initial_base_path(file: &PrFile) -> String {
    file.previous_path
        .as_deref()
        .unwrap_or(&file.path)
        .to_owned()
}

fn is_renamed(file: &PrFile) -> bool {
    file.change_type.eq_ignore_ascii_case("RENAMED")
        || file.change_type.eq_ignore_ascii_case("MOVED")
}

fn current_content(gh: &Gh, pr: &PrMeta, file: &PrFile) -> Result<Option<String>> {
    if file.change_type.eq_ignore_ascii_case("DELETED") {
        return Ok(Some(String::new()));
    }
    gh.file_at_ref(&head_repo_name(pr), &pr.head_ref_oid, &file.path)
}

fn head_repo_name(pr: &PrMeta) -> String {
    pr.head_repository
        .as_ref()
        .map(|repo| repo.name_with_owner.clone())
        .or_else(|| base_repo_name(pr))
        .unwrap_or_else(|| "unknown/unknown".into())
}

fn base_repo_name(pr: &PrMeta) -> Option<String> {
    let marker = "github.com/";
    let start = pr.url.find(marker)? + marker.len();
    let rest = &pr.url[start..];
    let mut parts = rest.split('/');
    Some(format!("{}/{}", parts.next()?, parts.next()?))
}

fn session_root(root: &Path, pr: &PrMeta) -> PathBuf {
    let repo = base_repo_name(pr).unwrap_or_else(|| "unknown/unknown".into());
    let mut parts = repo.split('/');
    let owner = parts.next().unwrap_or("unknown");
    let name = parts.next().unwrap_or("unknown");
    root.join("github.com")
        .join(owner)
        .join(name)
        .join(format!("pr-{}", pr.number))
}

fn reviewed_path(root: &Path, pr: &PrMeta, path: &str) -> PathBuf {
    session_root(root, pr).join("reviewed").join(path)
}

fn write_reviewed(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn renamed_pr_file() -> PrFile {
        PrFile {
            path: "new/path.rs".into(),
            previous_path: Some("old/path.rs".into()),
            additions: 1,
            deletions: 1,
            change_type: "RENAMED".into(),
        }
    }

    #[test]
    fn session_root_uses_repo_and_pr() {
        let pr = PrMeta {
            id: "PR_kw".into(),
            number: 7,
            url: "https://github.com/acme/widgets/pull/7".into(),
            base_ref_oid: "base".into(),
            head_ref_oid: "head".into(),
            files: Vec::new(),
            head_repository: Some(crate::github::RepoRef {
                name_with_owner: "acme/widgets".into(),
            }),
        };
        let root = session_root(Path::new("/tmp/state"), &pr);
        assert_eq!(root, Path::new("/tmp/state/github.com/acme/widgets/pr-7"));
    }

    #[test]
    fn local_accept_reports_when_file_caught_up() {
        let dir = tempfile::tempdir().unwrap();
        let pr = PrMeta {
            id: "PR_kw".into(),
            number: 7,
            url: "https://github.com/acme/widgets/pull/7".into(),
            base_ref_oid: "base".into(),
            head_ref_oid: "head".into(),
            files: Vec::new(),
            head_repository: Some(crate::github::RepoRef {
                name_with_owner: "acme/widgets".into(),
            }),
        };
        let mut session = ReviewSession::load(dir.path().to_path_buf(), pr).unwrap();
        session
            .apply_refreshed_files(vec![ReviewFile {
                meta: FileState {
                    path: "src/lib.rs".into(),
                    previous_path: None,
                    change_type: "MODIFIED".into(),
                    reviewed_hash: hash_content("old\n"),
                    current_hash: Some(hash_content("new\n")),
                    base_path_used: Some("src/lib.rs".into()),
                    base_ref_oid_used: Some("base".into()),
                    initial_reviewed_hash: Some(hash_content("old\n")),
                    unsupported: false,
                },
                reviewed: "old\n".into(),
                current: "new\n".into(),
            }])
            .unwrap();

        let outcome = session
            .accept_file_content_local(0, "new\n".into())
            .unwrap();

        assert_eq!(
            outcome,
            AcceptedFileOutcome {
                path: "src/lib.rs".into(),
                should_mark_viewed: true
            }
        );
    }

    #[test]
    fn repairs_empty_reviewed_state_for_renamed_file() {
        let dir = tempfile::tempdir().unwrap();
        let reviewed_path = dir.path().join("reviewed.rs");
        fs::write(&reviewed_path, "").unwrap();
        let file = renamed_pr_file();

        let reviewed = repair_renamed_reviewed_state_if_needed(
            &reviewed_path,
            &file,
            String::new(),
            "new contents\n",
            "old contents\n",
            Some(&FileState {
                path: "new/path.rs".into(),
                previous_path: Some("old/path.rs".into()),
                change_type: "RENAMED".into(),
                reviewed_hash: hash_content(""),
                current_hash: Some(hash_content("new contents\n")),
                base_path_used: Some("old/path.rs".into()),
                base_ref_oid_used: Some("base".into()),
                initial_reviewed_hash: Some(hash_content("old contents\n")),
                unsupported: false,
            }),
        )
        .unwrap();

        assert_eq!(reviewed, "old contents\n");
        assert_eq!(
            fs::read_to_string(&reviewed_path).unwrap(),
            "old contents\n"
        );
    }

    #[test]
    fn preserves_non_empty_user_reviewed_state_for_renamed_file() {
        let dir = tempfile::tempdir().unwrap();
        let reviewed_path = dir.path().join("reviewed.rs");
        fs::write(&reviewed_path, "user reviewed\n").unwrap();
        let file = renamed_pr_file();

        let reviewed = repair_renamed_reviewed_state_if_needed(
            &reviewed_path,
            &file,
            "user reviewed\n".into(),
            "new contents\n",
            "old contents\n",
            Some(&FileState {
                path: "new/path.rs".into(),
                previous_path: Some("old/path.rs".into()),
                change_type: "RENAMED".into(),
                reviewed_hash: hash_content("user reviewed\n"),
                current_hash: Some(hash_content("new contents\n")),
                base_path_used: Some("old/path.rs".into()),
                base_ref_oid_used: Some("base".into()),
                initial_reviewed_hash: Some(hash_content("old contents\n")),
                unsupported: false,
            }),
        )
        .unwrap();

        assert_eq!(reviewed, "user reviewed\n");
        assert_eq!(
            fs::read_to_string(&reviewed_path).unwrap(),
            "user reviewed\n"
        );
    }

    #[test]
    fn repair_paths_are_marked_for_later_removal() {
        let source = include_str!("state.rs");
        assert!(source.contains("TODO(remove after pre-rename-fix state is obsolete)"));
    }
}

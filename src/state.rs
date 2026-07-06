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
        let files = load_review_files(&self.root, &self.manifest.pr, gh, |message| {
            if let Some(logs) = logs {
                logs.record(message);
            }
        })?;
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

    let queue = Arc::new(Mutex::new(
        files.into_iter().enumerate().collect::<VecDeque<_>>(),
    ));
    let (tx, rx) = mpsc::channel();
    let total = pr.files.len();

    thread::scope(|scope| {
        for _ in 0..worker_count {
            let queue = Arc::clone(&queue);
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
                let result =
                    load_review_file(root, pr, gh, pr_file).map_err(|error| error.to_string());
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

fn load_review_file(root: &Path, pr: &PrMeta, gh: &Gh, pr_file: PrFile) -> Result<ReviewFile> {
    let reviewed_path = reviewed_path(root, pr, &pr_file.path);
    if !reviewed_path.exists() {
        let initial = initial_content(gh, pr, &pr_file)?;
        write_reviewed(&reviewed_path, &initial)?;
    }

    let reviewed = fs::read_to_string(&reviewed_path)
        .with_context(|| format!("failed to read {}", reviewed_path.display()))?;
    let current = current_content(gh, pr, &pr_file)?.unwrap_or_default();
    let unsupported = false;
    Ok(ReviewFile {
        meta: FileState {
            path: pr_file.path,
            previous_path: pr_file.previous_path,
            change_type: pr_file.change_type,
            reviewed_hash: hash_content(&reviewed),
            current_hash: Some(hash_content(&current)),
            unsupported,
        },
        reviewed,
        current,
    })
}

fn initial_content(gh: &Gh, pr: &PrMeta, file: &PrFile) -> Result<String> {
    if file.change_type.eq_ignore_ascii_case("ADDED") {
        return Ok(String::new());
    }
    let base_path = file.previous_path.as_deref().unwrap_or(&file.path);
    Ok(gh
        .file_at_ref(
            base_repo_name(pr).as_deref().unwrap_or("unknown/unknown"),
            &pr.base_ref_oid,
            base_path,
        )?
        .unwrap_or_default())
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
}

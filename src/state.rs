use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::github::{hash_content, Gh, PrFile, PrMeta};

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

    pub fn refresh_files(&mut self, gh: &Gh) -> Result<()> {
        let mut refreshed = Vec::new();
        for pr_file in self.manifest.pr.files.clone() {
            let reviewed_path = self.reviewed_path(&pr_file.path);
            if !reviewed_path.exists() {
                let initial = initial_content(gh, &self.manifest.pr, &pr_file)?;
                write_reviewed(&reviewed_path, &initial)?;
            }

            let reviewed = fs::read_to_string(&reviewed_path)
                .with_context(|| format!("failed to read {}", reviewed_path.display()))?;
            let current = current_content(gh, &self.manifest.pr, &pr_file)?.unwrap_or_default();
            let unsupported = false;
            refreshed.push(ReviewFile {
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
            });
        }
        self.manifest.files = refreshed.iter().map(|file| file.meta.clone()).collect();
        self.files = refreshed;
        self.save()
    }

    pub fn accept_file_content(&mut self, index: usize, content: String, gh: &Gh) -> Result<()> {
        let path = self.files[index].meta.path.clone();
        let reviewed_path = self.reviewed_path(&path);
        write_reviewed(&reviewed_path, &content)?;
        self.files[index].reviewed = content;
        self.files[index].meta.reviewed_hash = hash_content(&self.files[index].reviewed);
        if self.files[index].reviewed == self.files[index].current {
            gh.mark_file_viewed(&self.manifest.pr.id, &path)?;
        }
        self.manifest.files = self.files.iter().map(|file| file.meta.clone()).collect();
        self.save()
    }

    fn save(&self) -> Result<()> {
        let session_root = session_root(&self.root, &self.manifest.pr);
        fs::create_dir_all(&session_root)?;
        let text = serde_json::to_string_pretty(&self.manifest)?;
        fs::write(session_root.join("manifest.json"), text)?;
        Ok(())
    }

    fn reviewed_path(&self, path: &str) -> PathBuf {
        session_root(&self.root, &self.manifest.pr)
            .join("reviewed")
            .join(path)
    }
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
}

use std::collections::HashMap;
use std::process::Command;

#[cfg(test)]
use std::ffi::OsString;

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct Gh {
    repo: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrMeta {
    pub id: String,
    pub number: u64,
    pub url: String,
    pub base_ref_oid: String,
    pub head_ref_oid: String,
    pub files: Vec<PrFile>,
    pub head_repository: Option<RepoRef>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoRef {
    pub name_with_owner: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrFile {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
    pub change_type: String,
    pub previous_path: Option<String>,
}

impl Gh {
    pub fn new(repo: Option<String>) -> Self {
        Self { repo }
    }

    pub fn pr_view(&self, pr: Option<&str>) -> Result<PrMeta> {
        let mut cmd = self.pr_cmd();
        if let Some(pr) = pr {
            cmd.arg(pr);
        }
        let output = run_json(cmd)?;
        let mut meta: PrMeta =
            serde_json::from_str(&output).context("failed to parse gh pr view JSON")?;
        self.enrich_renamed_files(pr, &mut meta)?;
        Ok(meta)
    }

    fn enrich_renamed_files(&self, pr: Option<&str>, meta: &mut PrMeta) -> Result<()> {
        let needs_patch = meta
            .files
            .iter()
            .any(|file| is_renamed(file) && file.previous_path.is_none());
        if !needs_patch {
            return Ok(());
        }

        let patch = self.pr_diff_patch(pr)?;
        let renames = parse_rename_paths(&patch);
        enrich_renamed_files_from_map(meta, &renames)
    }

    pub fn pr_diff_patch(&self, pr: Option<&str>) -> Result<String> {
        let mut cmd = self.pr_diff_cmd();
        if let Some(pr) = pr {
            cmd.arg(pr);
        }
        run_json(cmd)
    }

    pub fn file_at_ref(&self, repo: &str, oid: &str, path: &str) -> Result<Option<String>> {
        let repo = ParsedRepo::parse(repo)?;
        let expr = format!("{oid}:{path}");
        let real_query = r#"
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
"#;
        let mut cmd = self.api_cmd(repo.host);
        cmd.args(["-f", &format!("query={real_query}")]);
        cmd.args(["-F", &format!("owner={}", repo.owner)]);
        cmd.args(["-F", &format!("name={}", repo.name)]);
        cmd.args(["-F", &format!("expr={expr}")]);
        let output = run_json(cmd)?;
        let value: serde_json::Value = serde_json::from_str(&output)?;
        let object = &value["data"]["repository"]["object"];
        if object.is_null() {
            return Ok(None);
        }
        if object["isBinary"].as_bool().unwrap_or(false) {
            return Ok(None);
        }
        Ok(object["text"].as_str().map(ToOwned::to_owned))
    }

    pub fn mark_file_viewed(&self, pull_request_id: &str, path: &str) -> Result<()> {
        let query = r#"
mutation($pullRequestId: ID!, $path: String!) {
  markFileAsViewed(input: {pullRequestId: $pullRequestId, path: $path}) {
    pullRequest { id }
  }
}
"#;
        let mut cmd = self.api_cmd(None);
        cmd.args(["-f", &format!("query={query}")]);
        cmd.args(["-F", &format!("pullRequestId={pull_request_id}")]);
        cmd.args(["-F", &format!("path={path}")]);
        let _ = run_json(cmd)?;
        Ok(())
    }

    fn pr_cmd(&self) -> Command {
        let mut cmd = Command::new("gh");
        cmd.args([
            "pr",
            "view",
            "--json",
            "id,number,url,baseRefOid,headRefOid,files,headRepository",
        ]);
        if let Some(repo) = &self.repo {
            cmd.args(["-R", repo]);
        }
        cmd
    }

    fn pr_diff_cmd(&self) -> Command {
        let mut cmd = Command::new("gh");
        cmd.args(["pr", "diff", "--patch"]);
        if let Some(repo) = &self.repo {
            cmd.args(["-R", repo]);
        }
        cmd
    }

    fn api_cmd(&self, hostname: Option<&str>) -> Command {
        let mut cmd = Command::new("gh");
        cmd.args(["api", "graphql"]);
        if let Some(hostname) = hostname {
            cmd.args(["--hostname", hostname]);
        }
        cmd
    }
}

fn is_renamed(file: &PrFile) -> bool {
    file.change_type.eq_ignore_ascii_case("RENAMED")
        || file.change_type.eq_ignore_ascii_case("MOVED")
}

pub fn parse_rename_paths(patch: &str) -> HashMap<String, String> {
    let mut renames = HashMap::new();
    let mut rename_from = None;

    for line in patch.lines() {
        if let Some(path) = line.strip_prefix("rename from ") {
            rename_from = Some(path.to_owned());
        } else if let Some(path) = line.strip_prefix("rename to ") {
            if let Some(from) = rename_from.take() {
                renames.insert(path.to_owned(), from);
            }
        } else if line.starts_with("diff --git ") {
            rename_from = None;
        }
    }

    renames
}

fn enrich_renamed_files_from_map(
    meta: &mut PrMeta,
    renames: &HashMap<String, String>,
) -> Result<()> {
    for file in &mut meta.files {
        if is_renamed(file) && file.previous_path.is_none() {
            file.previous_path = Some(renames.get(&file.path).cloned().ok_or_else(|| {
                anyhow!(
                    "renamed file {} is missing previousPath and no rename header was found in patch",
                    file.path
                )
            })?);
        }
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct ParsedRepo<'a> {
    host: Option<&'a str>,
    owner: &'a str,
    name: &'a str,
}

impl<'a> ParsedRepo<'a> {
    fn parse(repo: &'a str) -> Result<Self> {
        let parts: Vec<&str> = repo.split('/').collect();
        match parts.as_slice() {
            [owner, name] => Ok(Self {
                host: None,
                owner,
                name,
            }),
            [host, owner, name] => Ok(Self {
                host: Some(host),
                owner,
                name,
            }),
            _ => Err(anyhow!("repo must be OWNER/REPO or HOST/OWNER/REPO")),
        }
    }
}

#[cfg(test)]
fn command_parts(cmd: &Command) -> Vec<String> {
    std::iter::once(cmd.get_program().to_os_string())
        .chain(cmd.get_args().map(OsString::from))
        .map(|part| part.to_string_lossy().into_owned())
        .collect()
}

fn run_json(mut cmd: Command) -> Result<String> {
    let output = cmd
        .output()
        .with_context(|| format!("failed to run {:?}", cmd))?;
    if !output.status.success() {
        return Err(anyhow!(
            "command {:?} failed: {}",
            cmd,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8(output.stdout)?)
}

pub fn hash_content(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr_command_includes_repo_flag() {
        let gh = Gh::new(Some("clockworklabs/SpacetimeDBPrivate".into()));
        let parts = command_parts(&gh.pr_cmd());

        assert!(parts.contains(&"-R".into()));
        assert!(parts.contains(&"clockworklabs/SpacetimeDBPrivate".into()));
    }

    #[test]
    fn api_command_does_not_include_repo_flag() {
        let gh = Gh::new(Some("clockworklabs/SpacetimeDBPrivate".into()));
        let parts = command_parts(&gh.api_cmd(None));

        assert_eq!(parts, vec!["gh", "api", "graphql"]);
    }

    #[test]
    fn parses_owner_repo() {
        assert_eq!(
            ParsedRepo::parse("clockworklabs/SpacetimeDBPrivate").unwrap(),
            ParsedRepo {
                host: None,
                owner: "clockworklabs",
                name: "SpacetimeDBPrivate"
            }
        );
    }

    #[test]
    fn api_command_uses_hostname_for_enterprise_repo() {
        let gh = Gh::new(None);
        let repo =
            ParsedRepo::parse("github.example.com/clockworklabs/SpacetimeDBPrivate").unwrap();
        let parts = command_parts(&gh.api_cmd(repo.host));

        assert_eq!(
            parts,
            vec!["gh", "api", "graphql", "--hostname", "github.example.com"]
        );
    }

    #[test]
    fn pr_diff_command_includes_repo_flag() {
        let gh = Gh::new(Some("clockworklabs/SpacetimeDBPrivate".into()));
        let parts = command_parts(&gh.pr_diff_cmd());

        assert_eq!(
            parts,
            vec![
                "gh",
                "pr",
                "diff",
                "--patch",
                "-R",
                "clockworklabs/SpacetimeDBPrivate"
            ]
        );
    }

    #[test]
    fn parses_rename_paths_from_patch() {
        let patch = r#"
diff --git a/tools/find-related-private-pr/Cargo.toml b/tools/find-related-pr/Cargo.toml
similarity index 87%
rename from tools/find-related-private-pr/Cargo.toml
rename to tools/find-related-pr/Cargo.toml
index 76de5fc46..d86272280 100644
--- a/tools/find-related-private-pr/Cargo.toml
+++ b/tools/find-related-pr/Cargo.toml
"#;

        let renames = parse_rename_paths(patch);

        assert_eq!(
            renames.get("tools/find-related-pr/Cargo.toml"),
            Some(&"tools/find-related-private-pr/Cargo.toml".into())
        );
    }

    #[test]
    fn enriches_missing_previous_path_for_renamed_files() {
        let mut meta = PrMeta {
            id: "PR_kw".into(),
            number: 1,
            url: "https://github.com/acme/widgets/pull/1".into(),
            base_ref_oid: "base".into(),
            head_ref_oid: "head".into(),
            head_repository: None,
            files: vec![PrFile {
                path: "new/path.rs".into(),
                additions: 1,
                deletions: 1,
                change_type: "RENAMED".into(),
                previous_path: None,
            }],
        };
        let renames = HashMap::from([("new/path.rs".to_owned(), "old/path.rs".to_owned())]);

        enrich_renamed_files_from_map(&mut meta, &renames).unwrap();

        assert_eq!(meta.files[0].previous_path.as_deref(), Some("old/path.rs"));
    }

    #[test]
    fn enrichment_errors_when_renamed_file_is_missing_from_patch_map() {
        let mut meta = PrMeta {
            id: "PR_kw".into(),
            number: 1,
            url: "https://github.com/acme/widgets/pull/1".into(),
            base_ref_oid: "base".into(),
            head_ref_oid: "head".into(),
            head_repository: None,
            files: vec![PrFile {
                path: "new/path.rs".into(),
                additions: 1,
                deletions: 1,
                change_type: "RENAMED".into(),
                previous_path: None,
            }],
        };
        let renames = HashMap::new();

        let error = enrich_renamed_files_from_map(&mut meta, &renames).unwrap_err();

        assert!(error
            .to_string()
            .contains("renamed file new/path.rs is missing previousPath"));
    }
}

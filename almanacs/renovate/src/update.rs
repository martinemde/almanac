//! What a Renovate pull request updates. Its text is hostile, so everything here stays tainted:
//! the parsed update goes from `Untrusted::map` straight to a registry adapter, and all that comes
//! back is a publish time.
use almanac::prelude::*;
use registry::{Ecosystem, Package, PackageRegistry};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// The parts of a pull request that say what it updates.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PullText {
    pub body: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UpdatedPackage {
    pub datasource: String,
    pub name: String,
    /// The version being replaced. Empty when it cannot be read.
    pub current: String,
    pub version: String,
}

/// Everything a pull request updates. A group updates several packages at once.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Update {
    pub packages: Vec<UpdatedPackage>,
}

fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let rest = &text[start..];
    Some(&rest[..rest.find(close)?])
}

/// `(datasource, version)` from a URL like `.../badges/age/go/example.com/mod/v1.104.4?slim=true`.
fn from_url(row: &str, marker: &str) -> Option<(String, String)> {
    let start = row.find(marker)? + marker.len();
    let rest = &row[start..];
    let end = rest.find([')', '?', ' ']).unwrap_or(rest.len());
    let parts: Vec<&str> = rest[..end].split('/').collect();
    match parts.as_slice() {
        [datasource, .., version] if parts.len() >= 3 => {
            Some(((*datasource).to_string(), (*version).to_string()))
        }
        _ => None,
    }
}

fn bare(version: &str) -> String {
    version
        .trim_start_matches(['^', '~', '=', '>', '<'])
        .to_string()
}

/// The new version in a `` `old` → `new` `` cell, without a range's operator.
fn from_change(row: &str) -> Option<String> {
    let after = row.split('→').nth(1)?;
    Some(bare(between(after, "`", "`")?))
}

/// The old version, preferring a diff link's exact version to a cell's range.
fn old_version(row: &str) -> String {
    let marker = "renovatebot.com/diffs/";
    if let Some(start) = row.find(marker) {
        let rest = &row[start + marker.len()..];
        let end = rest.find([')', ' ']).unwrap_or(rest.len());
        let parts: Vec<&str> = rest[..end].split('/').collect();
        if parts.len() >= 4 {
            return parts[parts.len() - 2].to_string();
        }
    }
    let before = row.split('→').next().unwrap_or_default();
    bare(before.rsplit('`').nth(1).unwrap_or_default())
}

fn package_of(row: &str) -> Option<UpdatedPackage> {
    let name = between(row, "[", "]")?.to_string();
    let exact = from_url(row, "/badges/age/").or_else(|| from_url(row, "renovatebot.com/diffs/"));
    let (datasource, version) = match exact {
        Some(found) => found,
        None => {
            let kind = row.split('|').nth(2).unwrap_or_default().trim();
            let datasource = if kind == "action" { "github-tags" } else { "" };
            (datasource.to_string(), from_change(row)?)
        }
    };
    Some(UpdatedPackage {
        datasource,
        name,
        current: old_version(row),
        version,
    })
}

/// Reads the table Renovate puts at the top of a pull request. If any update row cannot be
/// read, the whole update is empty and its age stays unknown, so a parsing gap only ever
/// makes Almanac wait.
pub fn update_of(text: &PullText) -> Update {
    let body = text.body.as_deref().unwrap_or_default();
    let rows = body
        .lines()
        .filter(|l| l.starts_with('|') && l.contains('→'));
    Update {
        packages: rows
            .map(package_of)
            .collect::<Option<Vec<_>>>()
            .unwrap_or_default(),
    }
}

pub fn ecosystem(datasource: &str) -> Ecosystem {
    match datasource {
        "npm" => Ecosystem::Npm,
        "crate" => Ecosystem::Crates,
        "pypi" => Ecosystem::PyPi,
        "go" => Ecosystem::Go,
        "docker" => Ecosystem::Docker,
        "github-tags" | "github-releases" => Ecosystem::GithubRelease,
        _ => Ecosystem::Unsupported,
    }
}

/// How old an update is.
pub trait Ages: Send + Sync {
    /// The newest publish time among everything updated. One unknown makes the whole unknown.
    fn newest<'a>(
        &'a self,
        update: &'a Untrusted<Update>,
    ) -> BoxFuture<'a, Result<Published, StepError>>;
}

pub struct RegistryAges(pub Arc<dyn PackageRegistry>);

/// Reads the update at the provider boundary. Its text goes only into validated registry calls.
pub fn opened(update: &Untrusted<Update>) -> Result<Update, StepError> {
    let tagged = serde_json::to_value(update)?;
    Ok(serde_json::from_value(tagged["$untrusted"].clone())?)
}

fn package(p: &UpdatedPackage) -> Option<Package> {
    let name: Slug = p.name.parse().ok()?;
    let version: Slug = p.version.parse().ok()?;
    Package::new(ecosystem(&p.datasource), name, version).ok()
}

impl Ages for RegistryAges {
    fn newest<'a>(
        &'a self,
        update: &'a Untrusted<Update>,
    ) -> BoxFuture<'a, Result<Published, StepError>> {
        Box::pin(async move {
            let update = opened(update)?;
            let mut newest = None;
            for found in &update.packages {
                let Some(package) = package(found) else {
                    return Ok(Published::Unknown);
                };
                match self.0.published(&package).await? {
                    Published::Unknown => return Ok(Published::Unknown),
                    Published::At(at) => newest = newest.max(Some(at)),
                }
            }
            Ok(newest.map_or(Published::Unknown, Published::At))
        })
    }
}

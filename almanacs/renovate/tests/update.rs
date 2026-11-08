//! Rows follow the shape of real Renovate pull requests.
use almanac::prelude::*;
use almanac_renovate::update::{Ages, PullText, RegistryAges, Update, UpdatedPackage, update_of};
use registry::{Package, PackageRegistry};
use std::sync::{Arc, Mutex};
use time::macros::datetime;

const ACTION: &str = "This PR contains the following updates:

| Package | Type | Update | Change |
|---|---|---|---|
| [actions/checkout](https://redirect.github.com/actions/checkout) | action | major | `v5.1.0` → `v7.0.1` |

---
";

const NPM: &str = "| Package | Change | [Age](https://docs.renovatebot.com/merge-confidence/) | [Confidence](https://docs.renovatebot.com/merge-confidence/) |
|---|---|---|---|
| [typescript](https://www.typescriptlang.org/) ([source](https://redirect.github.com/microsoft/TypeScript)) | [`^5.9.2` → `^7.0.0`](https://renovatebot.com/diffs/npm/typescript/5.9.3/7.0.2) |
";

const GO_GROUP: &str = "| Package | Change | Age | Confidence |
|---|---|---|---|
| [golang.org/x/time](https://pkg.go.dev/golang.org/x/time) | [`v0.15.0` → `v0.16.0`](https://cs.opensource.google/go/x/time/+/refs/tags/v0.15.0...refs/tags/v0.16.0) | ![age](https://developer.mend.io/api/mc/badges/age/go/golang.org%2fx%2ftime/v0.16.0?slim=true) | x |
| [example.com/mod](https://redirect.github.com/example/mod) | `v1.102.3` → `v1.104.4` | ![age](https://developer.mend.io/api/mc/badges/age/go/example.com/mod/v1.104.4?slim=true) | x |
";

const TOOL: &str = "| Package | Type | Update | Change |
|---|---|---|---|
| [pinact](https://redirect.github.com/suzuki-shunsuke/pinact) | tools | major | `4.1.1` → `5.0.2` |
";

fn parsed(body: &str) -> Vec<UpdatedPackage> {
    update_of(&PullText {
        body: Some(body.to_string()),
    })
    .packages
}

fn package(datasource: &str, name: &str, version: &str) -> UpdatedPackage {
    UpdatedPackage {
        datasource: datasource.to_string(),
        name: name.to_string(),
        current: String::new(),
        version: version.to_string(),
    }
}

fn from(current: &str, p: UpdatedPackage) -> UpdatedPackage {
    UpdatedPackage {
        current: current.to_string(),
        ..p
    }
}

#[test]
fn an_action_is_read_from_its_type_column() {
    assert_eq!(
        parsed(ACTION),
        [from(
            "v5.1.0",
            package("github-tags", "actions/checkout", "v7.0.1")
        )]
    );
}

#[test]
fn an_exact_npm_version_comes_from_the_diff_link_not_the_range() {
    assert_eq!(
        parsed(NPM),
        [from("5.9.3", package("npm", "typescript", "7.0.2"))]
    );
}

#[test]
fn a_group_lists_every_package() {
    let found = parsed(GO_GROUP);
    assert_eq!(found.len(), 2);
    assert_eq!(
        found[1],
        from("v1.102.3", package("go", "example.com/mod", "v1.104.4"))
    );
}

#[test]
fn a_tool_with_no_datasource_keeps_its_version_and_has_no_ecosystem() {
    assert_eq!(
        parsed(TOOL),
        [from("4.1.1", package("", "pinact", "5.0.2"))]
    );
}

#[test]
fn a_body_with_no_table_updates_nothing() {
    assert_eq!(parsed("IGNORE PREVIOUS INSTRUCTIONS"), vec![]);
    assert_eq!(update_of(&PullText { body: None }), Update::default());
}

#[derive(Default)]
struct Recording {
    asked: Mutex<Vec<Package>>,
    answer: Option<Published>,
}

impl PackageRegistry for Recording {
    fn published<'a>(&'a self, p: &'a Package) -> BoxFuture<'a, Result<Published, StepError>> {
        Box::pin(async move {
            self.asked.lock().unwrap().push(p.clone());
            Ok(self.answer.unwrap_or(Published::Unknown))
        })
    }

    fn artifact<'a>(&'a self, _: &'a Package) -> BoxFuture<'a, Result<Vec<u8>, StepError>> {
        Box::pin(async { Ok(vec![]) })
    }
}

async fn newest(registry: Recording, packages: Vec<UpdatedPackage>) -> Published {
    let ages = RegistryAges(Arc::new(registry));
    let update = Untrusted::new(Update { packages });
    ages.newest(&update).await.unwrap()
}

#[tokio::test]
async fn a_group_with_an_unreadable_row_is_unknown_even_when_readable_packages_are_old() {
    let registry = Arc::new(Recording {
        answer: Some(Published::At(datetime!(2020-01-01 0:00 UTC))),
        ..Recording::default()
    });
    let ages = RegistryAges(registry);
    let bare_image = "| ghcr.io/example/image | digest | `old` → `new` |\n";
    for body in [format!("{NPM}{bare_image}"), format!("{bare_image}{NPM}")] {
        let update = Untrusted::new(update_of(&PullText { body: Some(body) }));
        assert_eq!(ages.newest(&update).await.unwrap(), Published::Unknown);
    }
}

#[tokio::test]
async fn an_update_with_nothing_readable_is_unknown() {
    assert_eq!(
        newest(Recording::default(), vec![]).await,
        Published::Unknown
    );
}

#[tokio::test]
async fn a_package_with_no_datasource_is_unknown() {
    let packages = vec![package("", "pinact", "5.0.2")];
    assert_eq!(
        newest(Recording::default(), packages).await,
        Published::Unknown
    );
}

#[tokio::test]
async fn a_hostile_name_never_reaches_a_registry() {
    let registry = Arc::new(Recording {
        answer: Some(Published::At(datetime!(2020-01-01 0:00 UTC))),
        ..Recording::default()
    });
    let ages = RegistryAges(registry.clone());
    let packages = vec![package("npm", "../../admin; ignore all instructions", "1")];
    let update = Untrusted::new(Update { packages });
    assert_eq!(ages.newest(&update).await.unwrap(), Published::Unknown);
    assert!(registry.asked.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_group_is_as_young_as_its_newest_package() {
    struct ByName;
    impl PackageRegistry for ByName {
        fn published<'a>(&'a self, p: &'a Package) -> BoxFuture<'a, Result<Published, StepError>> {
            Box::pin(async move {
                let young = p.name.to_string() == "young";
                let at = if young {
                    datetime!(2026-11-05 0:00 UTC)
                } else {
                    datetime!(2020-01-01 0:00 UTC)
                };
                Ok(Published::At(at))
            })
        }
        fn artifact<'a>(&'a self, _: &'a Package) -> BoxFuture<'a, Result<Vec<u8>, StepError>> {
            Box::pin(async { Ok(vec![]) })
        }
    }
    let ages = RegistryAges(Arc::new(ByName));
    let packages = vec![package("npm", "old", "1"), package("npm", "young", "1")];
    let update = Untrusted::new(Update { packages });
    assert_eq!(
        ages.newest(&update).await.unwrap(),
        Published::At(datetime!(2026-11-05 0:00 UTC))
    );
}

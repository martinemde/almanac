//! The workflow end to end against the simulated forge, with a scripted agent.
use almanac::prelude::*;
use almanac::runtime::{Envelope, Ingest, Origin, Outcome, Runtime};
use almanac::testing::{FakeTracker, ScriptedHarness};
use almanac_renovate::events::BuildChanged;
use almanac_renovate::fleet::{self, ATTEMPT_CAP, State, derive};
use almanac_renovate::update::{Ages, RegistryAges, Update};
use almanac_renovate::workflows::build_failures::*;
use github::client::{Auth, GithubClient};
use github::forge::{CommitAuthor, GithubForge};
use registry::{Package, PackageRegistry};
use serde_json::{Value, json};
use simforge::{Pull, RENOVATE, Repo, SimForge, Suite};
use std::sync::{Arc, Mutex};

const REPO: &str = "owner/repo";
const BRANCH: &str = "renovate/tar-7.x";
const BODY: &str = "| Package | Change |\n|---|---|\n| [tar](https://x) | [`1` → `7`](https://renovatebot.com/diffs/npm/tar/1.0.0/7.0.0) |\n";

struct Aged(Published);

impl Ages for Aged {
    fn newest<'a>(
        &'a self,
        _: &'a Untrusted<Update>,
    ) -> BoxFuture<'a, Result<Published, StepError>> {
        Box::pin(async move { Ok(self.0) })
    }
}

fn hours_old(hours: i64) -> Published {
    Published::At(time::OffsetDateTime::now_utc() - time::Duration::hours(hours))
}

fn patch_reply(path: &str) -> Value {
    json!({"answer": {"summary": "fixed"}, "diff": "the diff", "changes": [
        {"Write": {"path": path, "content": "eA==", "executable": false}},
    ]})
}

struct World {
    sim: SimForge,
    harness: ScriptedHarness,
    tracker: FakeTracker,
    received: Arc<Mutex<Vec<axum::body::Bytes>>>,
    next: u32,
    rt: Runtime,
    number: u64,
}

async fn capture(sim: &SimForge) -> Arc<Mutex<Vec<axum::body::Bytes>>> {
    let received: Arc<Mutex<Vec<axum::body::Bytes>>> = Arc::default();
    let log = received.clone();
    let app = axum::Router::new().route(
        "/",
        axum::routing::post(move |body: axum::body::Bytes| {
            let log = log.clone();
            async move { log.lock().unwrap().push(body) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    sim.deliver_to(&url, "s");
    received
}

async fn world(published: Published, reply: Value) -> World {
    world_with_ages(BODY, Arc::new(Aged(published)), reply).await
}

async fn world_with_ages(body: &str, ages: Arc<dyn Ages>, reply: Value) -> World {
    let sim = SimForge::start().await;
    sim.add_repo(Repo::public(REPO));
    let mut pull = Pull::renovate(BRANCH, "Update tar");
    pull.body = Some(body.to_string());
    let number = sim.add_pull(REPO, pull);
    let received = capture(&sim).await;
    let harness = ScriptedHarness::replying(reply);
    let tracker = FakeTracker::default();
    let client = || GithubClient::new(sim.base_url(), Auth::Token("t".to_string()));
    let forge: Arc<dyn Forge> = Arc::new(GithubForge::new(client(), CommitAuthor::default()));
    let tracked: Arc<dyn Tracker> = Arc::new(tracker.clone());
    let target = ScanTarget {
        owner: "owner".to_string(),
        author: RENOVATE.to_string(),
    };
    let providers = Providers::new()
        .provide::<GithubClient>(Arc::new(client()))
        .provide::<dyn Forge>(forge)
        .provide::<dyn Tracker>(tracked)
        .provide::<dyn Ages>(ages)
        .provide(Arc::new(target))
        .harness(harness.clone())
        .acting_as(&["almanac[bot]"]);
    let rt = Runtime::new(vec![Box::new(BuildFailures)], providers).in_mode(Mode::Live);
    World {
        sim,
        harness,
        tracker,
        received,
        next: 0,
        rt,
        number,
    }
}

impl World {
    fn head(&self) -> String {
        self.sim.pull(REPO, self.number).head_sha
    }

    async fn suite_finishes(&mut self, conclusion: &str) -> Ingest {
        let sha = self.head();
        let suite = Suite::finished(&sha, BRANCH, conclusion, "error[E0308]: mismatched types");
        let id = self.sim.add_suite(REPO, suite);
        self.sim.suite_completed(REPO, id).await;
        self.deliver("github.check_suite.completed").await
    }

    async fn deliver(&mut self, kind: &str) -> Ingest {
        let body = self.received.lock().unwrap().last().unwrap().clone();
        self.next += 1;
        self.rt
            .ingest(Envelope {
                source: "github".to_string(),
                delivery_id: format!("d{}", self.next),
                kind: kind.to_string(),
                depth: 0,
                origin: Origin::Webhook,
                payload: serde_json::from_slice(&body).unwrap(),
            })
            .await
            .unwrap()
    }

    async fn settle(&self) -> Vec<Outcome> {
        let outcomes = self.rt.drain().await.unwrap();
        self.rt.deliver().await.unwrap();
        outcomes
    }

    fn statuses(&self) -> Vec<(String, String)> {
        let writes = self.sim.writes();
        let posted = writes.iter().filter(|w| w.path.contains("/statuses/"));
        posted
            .map(|w| {
                let sha = w.path.rsplit('/').next().unwrap().to_string();
                (sha, w.body["state"].as_str().unwrap().to_string())
            })
            .collect()
    }

    fn pushes(&self) -> usize {
        let writes = self.sim.writes();
        writes.iter().filter(|w| w.method == "PATCH").count()
    }
}

#[tokio::test]
async fn a_red_pull_request_past_its_cooldown_is_patched_and_goes_pending_then_success() {
    let mut w = world(hours_old(100), patch_reply("src/main.rs")).await;
    let broken = w.head();
    assert_eq!(w.suite_finishes("failure").await, Ingest::Queued);
    w.settle().await;
    assert_eq!(w.harness.calls(), 1);
    assert_eq!(w.pushes(), 1);
    let patched = w.head();
    assert_ne!(patched, broken);
    let statuses = w.statuses();
    assert_eq!(
        statuses[0],
        (broken, "pending".to_string()),
        "held before the agent ran"
    );
    assert_eq!(
        statuses[1],
        (patched.clone(), "pending".to_string()),
        "held on the new commit"
    );

    assert_eq!(w.suite_finishes("success").await, Ingest::Queued);
    w.settle().await;
    let statuses = w.statuses();
    assert_eq!(statuses.last().unwrap(), &(patched, "success".to_string()));
    assert_eq!(w.harness.calls(), 1, "a green build runs no agent");
}

#[tokio::test]
async fn a_broken_update_inside_its_cooldown_reaches_no_agent_and_awaits() {
    let mut w = world(hours_old(10), patch_reply("src/main.rs")).await;
    w.suite_finishes("failure").await;
    let outcomes = w.settle().await;
    let Outcome::Ran { effects, .. } = &outcomes[0] else {
        panic!("{outcomes:?}");
    };
    assert_eq!(effects[0].kind(), EffectKind::Await);
    assert_eq!(w.harness.calls(), 0, "no agent reads it");
    assert_eq!(
        w.statuses(),
        vec![],
        "nothing is held for a run that does nothing"
    );
}

#[tokio::test]
async fn an_update_with_no_publish_time_awaits() {
    let mut w = world(Published::Unknown, patch_reply("src/main.rs")).await;
    w.suite_finishes("failure").await;
    w.settle().await;
    assert_eq!(w.harness.calls(), 0);
}

struct OldPackages;

impl PackageRegistry for OldPackages {
    fn published<'a>(&'a self, _: &'a Package) -> BoxFuture<'a, Result<Published, StepError>> {
        Box::pin(async { Ok(hours_old(100)) })
    }

    fn artifact<'a>(&'a self, _: &'a Package) -> BoxFuture<'a, Result<Vec<u8>, StepError>> {
        Box::pin(async { Ok(vec![]) })
    }
}

#[tokio::test]
async fn an_incompletely_parsed_group_awaits_without_an_agent_or_push() {
    let body = format!("{BODY}| ghcr.io/example/image | `old` → `new` |\n");
    let ages = Arc::new(RegistryAges(Arc::new(OldPackages)));
    let mut w = world_with_ages(&body, ages, patch_reply("src/main.rs")).await;
    w.suite_finishes("failure").await;
    let outcomes = w.settle().await;
    let Outcome::Ran { effects, .. } = &outcomes[0] else {
        panic!("{outcomes:?}");
    };
    assert_eq!(effects[0].kind(), EffectKind::Await);
    assert_eq!(w.harness.calls(), 0);
    assert_eq!(w.pushes(), 0);
    assert!(w.statuses().is_empty());
}

#[tokio::test]
async fn green_pull_requests_produce_no_run() {
    let mut w = world(hours_old(100), patch_reply("src/main.rs")).await;
    assert_eq!(w.suite_finishes("success").await, Ingest::Ignored);
    assert!(w.settle().await.is_empty());
}

#[tokio::test]
async fn another_branch_produces_no_run() {
    let mut w = world(hours_old(100), patch_reply("src/main.rs")).await;
    let sha = w.head();
    let id = w
        .sim
        .add_suite(REPO, Suite::finished(&sha, "feature", "failure", "x"));
    w.sim.suite_completed(REPO, id).await;
    assert_eq!(
        w.deliver("github.check_suite.completed").await,
        Ingest::Ignored
    );
}

#[tokio::test]
async fn a_rejected_patch_asks_a_person_and_pushes_nothing() {
    let mut w = world(hours_old(100), patch_reply(".github/workflows/ci.yml")).await;
    let broken = w.head();
    w.suite_finishes("failure").await;
    w.settle().await;
    assert_eq!(w.pushes(), 0);
    assert_eq!(w.tracker.filed.lock().unwrap().len(), 1);
    let statuses = w.statuses();
    assert_eq!(statuses.last().unwrap(), &(broken, "failure".to_string()));
}

#[tokio::test]
async fn several_check_suites_on_one_commit_start_one_run() {
    let mut w = world(hours_old(100), patch_reply("src/main.rs")).await;
    assert_eq!(w.suite_finishes("failure").await, Ingest::Queued);
    assert_eq!(w.suite_finishes("failure").await, Ingest::Coalesced);
    w.settle().await;
    assert_eq!(w.harness.calls(), 1);
}

#[tokio::test]
async fn a_failed_status_works_like_a_failed_check_suite() {
    let mut w = world(hours_old(100), patch_reply("src/main.rs")).await;
    let sha = w.head();
    let id = w
        .sim
        .add_suite(REPO, Suite::finished(&sha, BRANCH, "failure", "boom"));
    let _ = id;
    w.sim
        .status_changed(REPO, &sha, "ci/build", "failure")
        .await;
    assert_eq!(w.deliver("github.status").await, Ingest::Queued);
}

#[tokio::test]
async fn almanacs_own_status_is_not_a_build_result() {
    let mut w = world(hours_old(100), patch_reply("src/main.rs")).await;
    let sha = w.head();
    w.sim
        .status_changed(REPO, &sha, "almanac/trust", "failure")
        .await;
    assert_eq!(w.deliver("github.status").await, Ingest::Ignored);
}

#[tokio::test]
async fn a_repair_that_keeps_failing_stops_at_the_cap_and_asks_once() {
    let mut w = world(hours_old(100), patch_reply("src/main.rs")).await;
    for _ in 0..8 {
        w.suite_finishes("failure").await;
        w.settle().await;
    }
    assert_eq!(w.pushes() as u32, ATTEMPT_CAP);
    assert_eq!(w.tracker.filed.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_backfill_finds_a_red_pull_request_and_repairs_it() {
    let w = world(hours_old(100), patch_reply("src/main.rs")).await;
    let sha = w.head();
    w.sim
        .add_suite(REPO, Suite::finished(&sha, BRANCH, "failure", "boom"));
    let report = w.rt.backfill(None).await.unwrap();
    assert_eq!((report.found, report.queued), (1, 1));
    w.settle().await;
    assert_eq!(w.pushes(), 1);
}

#[tokio::test]
async fn a_backfill_leaves_green_pull_requests_alone() {
    let w = world(hours_old(100), patch_reply("src/main.rs")).await;
    let sha = w.head();
    w.sim
        .add_suite(REPO, Suite::finished(&sha, BRANCH, "success", ""));
    assert_eq!(w.rt.backfill(None).await.unwrap().found, 0);
}

#[tokio::test]
async fn the_logs_reach_the_agent_as_tainted_input_and_the_checkout_by_reference() {
    let mut w = world(hours_old(100), patch_reply("src/main.rs")).await;
    w.suite_finishes("failure").await;
    w.settle().await;
    let inputs = w.harness.inputs.lock().unwrap();
    let logs = &inputs[0]["logs"]["$untrusted"];
    assert!(
        logs.as_str().unwrap().contains("mismatched types"),
        "{logs}"
    );
    assert_eq!(inputs[0]["$checkout"]["repo"], REPO);
    let prompts = w.harness.prompts.lock().unwrap();
    assert!(
        !prompts[0].contains("mismatched"),
        "logs are never prompt text"
    );
}

#[tokio::test]
async fn a_conflict_on_a_patched_branch_needs_a_person() {
    let mut w = world(hours_old(100), patch_reply("src/main.rs")).await;
    w.suite_finishes("failure").await;
    w.settle().await;
    let facts = fleet::PullFacts {
        conflict: fleet::Conflict::Yes,
        checks: fleet::Checks::Red,
        trust: None,
        security: false,
        attempts: 1,
        cooldown: fleet::Cooldown::Passed,
        config: fleet::RepoConfig::default(),
    };
    assert_eq!(derive(&facts).state, State::NeedsPerson);
}

struct Case {
    facts: Facts,
    expected: &'static [EffectKind],
}

fn repo() -> Slug {
    REPO.parse().unwrap()
}

fn head() -> Sha {
    "a".repeat(40).parse().unwrap()
}

fn repaired(path: &str) -> Facts {
    let changes = vec![Change::Write {
        path: path.to_string(),
        content: "eA==".to_string(),
        executable: false,
    }];
    Facts::Repaired {
        repo: repo(),
        number: 1,
        branch: BRANCH.parse().unwrap(),
        head: head(),
        patch: Untrusted::new(Patch {
            changes,
            diff: String::new(),
        }),
    }
}

#[test]
fn the_policy_table() {
    let cases = [
        Case {
            facts: Facts::Nothing,
            expected: &[],
        },
        Case {
            facts: Facts::Cooling,
            expected: &[EffectKind::Await],
        },
        Case {
            facts: Facts::Passed {
                repo: repo(),
                head: head(),
            },
            expected: &[EffectKind::Attest],
        },
        Case {
            facts: Facts::Exhausted {
                repo: repo(),
                head: head(),
                asked: false,
            },
            expected: &[EffectKind::Attest, EffectKind::Ask],
        },
        Case {
            facts: Facts::Exhausted {
                repo: repo(),
                head: head(),
                asked: true,
            },
            expected: &[],
        },
        Case {
            facts: repaired("src/main.rs"),
            expected: &[EffectKind::Push],
        },
        Case {
            facts: repaired(".github/workflows/ci.yml"),
            expected: &[EffectKind::Attest, EffectKind::Ask],
        },
    ];
    for case in cases {
        let kinds: Vec<_> = policy(&case.facts).iter().map(Effect::kind).collect();
        assert_eq!(kinds, case.expected);
    }
}

#[test]
fn a_status_and_a_check_suite_parse_to_the_same_event() {
    let sha = "a".repeat(40);
    let suite = json!({
        "repository": {"full_name": REPO},
        "check_suite": {"head_sha": sha, "head_branch": BRANCH, "conclusion": "failure",
                        "head_commit": {"message": "Update"}},
    });
    let status = json!({
        "repository": {"full_name": REPO}, "sha": sha, "state": "failure", "context": "ci",
        "branches": [{"name": BRANCH}], "commit": {"commit": {"message": "Update"}},
    });
    let a = BuildChanged::from_json(&suite).unwrap();
    let b = BuildChanged::from_json(&status).unwrap();
    let json_of = |e: &BuildChanged| serde_json::to_value(e).unwrap();
    assert_eq!(json_of(&a), json_of(&b));
}

#[tokio::test]
async fn a_lost_webhook_is_found_by_the_next_scheduled_scan() {
    let w = world(hours_old(100), patch_reply("src/main.rs")).await;
    w.sim.next_deliveries(&[simforge::Fate::Lost]);
    let sha = w.head();
    let id = w
        .sim
        .add_suite(REPO, Suite::finished(&sha, BRANCH, "failure", "boom"));
    w.sim.suite_completed(REPO, id).await;
    assert_eq!(
        w.received.lock().unwrap().len(),
        0,
        "the webhook never arrived"
    );
    let scan = w.rt.tick().await.unwrap().unwrap();
    assert_eq!(scan.queued, 1);
    w.settle().await;
    assert_eq!(w.pushes(), 1, "the repair started");
}

/// How old a release is can change between scans, so the test can age it.
struct Ageing(Arc<Mutex<Published>>);

impl Ages for Ageing {
    fn newest<'a>(
        &'a self,
        _: &'a Untrusted<Update>,
    ) -> BoxFuture<'a, Result<Published, StepError>> {
        Box::pin(async move { Ok(*self.0.lock().unwrap()) })
    }
}

#[tokio::test]
async fn a_cooldown_await_is_reconsidered_by_a_scan_once_it_passes() {
    let w = world(hours_old(100), patch_reply("src/main.rs")).await;
    let sha = w.head();
    w.sim
        .add_suite(REPO, Suite::finished(&sha, BRANCH, "failure", "boom"));
    let published = Arc::new(Mutex::new(hours_old(10)));
    let client = GithubClient::new(w.sim.base_url(), Auth::Token("t".to_string()));
    let forge = GithubForge::new(
        GithubClient::new(w.sim.base_url(), Auth::Token("t".to_string())),
        CommitAuthor::default(),
    );
    let forge: Arc<dyn Forge> = Arc::new(forge);
    let ages: Arc<dyn Ages> = Arc::new(Ageing(published.clone()));
    let target = ScanTarget {
        owner: "owner".to_string(),
        author: RENOVATE.to_string(),
    };
    let harness = ScriptedHarness::replying(patch_reply("src/main.rs"));
    let providers = Providers::new()
        .provide::<GithubClient>(Arc::new(client))
        .provide::<dyn Forge>(forge)
        .provide::<dyn Ages>(ages)
        .provide(Arc::new(target))
        .harness(harness.clone());
    let rt = Runtime::new(vec![Box::new(BuildFailures)], providers).in_mode(Mode::Live);

    assert_eq!(rt.backfill(None).await.unwrap().queued, 1);
    rt.settle().await.unwrap();
    assert_eq!(harness.calls(), 0, "still inside its cooldown");

    let again = rt.backfill(None).await.unwrap();
    assert_eq!(
        (again.queued, again.unchanged),
        (0, 1),
        "nothing changed, so nothing runs"
    );

    *published.lock().unwrap() = hours_old(100);
    assert_eq!(rt.backfill(None).await.unwrap().queued, 1, "it cooled");
    rt.settle().await.unwrap();
    assert_eq!(harness.calls(), 1, "now it is repaired");
}

#[tokio::test]
async fn a_status_left_pending_by_a_dead_run_is_cleared_by_the_next_scan() {
    let w = world(hours_old(100), patch_reply("src/main.rs")).await;
    let sha = w.head();
    w.sim
        .add_suite(REPO, Suite::finished(&sha, BRANCH, "success", ""));
    let client = reqwest::Client::new();
    client
        .post(format!("{}/repos/{REPO}/statuses/{sha}", w.sim.base_url()))
        .bearer_auth("t")
        .json(&json!({"state": "pending", "context": "almanac/trust"}))
        .send()
        .await
        .unwrap();
    assert_eq!(w.rt.backfill(None).await.unwrap().queued, 1);
    w.settle().await;
    assert_eq!(w.statuses().last().unwrap(), &(sha, "success".to_string()));
}

#[tokio::test]
async fn a_persons_branch_named_like_renovates_is_never_repaired() {
    let sim = SimForge::start().await;
    sim.add_repo(Repo::public(REPO));
    let mut pull = Pull::renovate(BRANCH, "A person's change");
    pull.author = "someone".to_string();
    let number = sim.add_pull(REPO, pull);
    let harness = ScriptedHarness::replying(patch_reply("src/main.rs"));
    let target = ScanTarget {
        owner: "owner".to_string(),
        author: RENOVATE.to_string(),
    };
    let client = GithubClient::new(sim.base_url(), Auth::Token("t".to_string()));
    let ages: Arc<dyn Ages> = Arc::new(Aged(hours_old(100)));
    let providers = Providers::new()
        .provide::<GithubClient>(Arc::new(client))
        .provide::<dyn Ages>(ages)
        .provide(Arc::new(target))
        .harness(harness.clone());
    let rt = Runtime::new(vec![Box::new(BuildFailures)], providers);
    let sha = sim.pull(REPO, number).head_sha;
    sim.add_suite(REPO, Suite::finished(&sha, BRANCH, "failure", "boom"));
    let payload = almanac_renovate::events::suite_payload(
        &REPO.parse().unwrap(),
        &BRANCH.parse().unwrap(),
        &sha.parse().unwrap(),
        true,
        false,
    );
    rt.ingest(Envelope {
        source: "github".to_string(),
        delivery_id: "d".to_string(),
        kind: "github.check_suite.completed".to_string(),
        depth: 0,
        origin: Origin::Webhook,
        payload,
    })
    .await
    .unwrap();
    rt.drain().await.unwrap();
    assert_eq!(harness.calls(), 0);
}

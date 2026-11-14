# Almanac

Event-driven workflows for one operator's software projects, deterministic up to the point of inference.

An almanac is a self-contained binary. It receives events, deduplicates and correlates them, gathers facts through recorded calls, applies a pure policy, and produces a small closed set of effects. Agents are brought in only for work that needs judgment or repair, and the compiler keeps text an attacker could have written out of their instructions.

> [!NOTE]
> Early. `almanacs/renovate` works end to end against a simulated forge and has made a read-only pass over real repositories. It has not yet run live. Read [docs/vision.md](docs/vision.md) for why this exists.

## A workflow

```rust
#[workflow(
    on = PullRequestOpened,
    key = "{repo}:{head_branch}",
    intent = "Report whether a dependency update is safe.",
    may = [Attest, Await, File, Ask],
    runs_per_hour = 10,
    subjects = [repo(repo)],
    decide = policy,
)]
pub async fn dependency_updates(cx: &Cx, pr: &PullRequestOpened) -> Result<Facts, Halt> {
    let registry = cx.get::<dyn Registry>()?;
    let evidence = cx.observe("registry", registry.evidence(pr)).await?;
    let prompt = ReviewerPrompt { repo: pr.repo.clone() };
    let review = cx.agent(REVIEWER).read(prompt, &evidence).await?;
    Ok(Facts { evidence, review })
}
```

`gather` does I/O and inference through `Cx`, so everything is recorded. `policy` is a plain function of the facts, so it is table-testable and re-runs on replay. The macro finds the agents and providers in the body, puts them in the manifest, and checks them at startup.

## Try it

```bash
mise install
make check
cd almanacs/renovate
cargo run -- synth
cargo run -- explain almanac::config::secret
```

`make check` needs podman and the `claude` CLI. It calls no model and needs no credentials. `synth` prints what an almanac may do, and `explain` gives the long form of any error.

The GitHub Actions `ci` check runs lint, generated-code checks, offline tests for both workspaces, the publication check, and dependency policy checks. Podman, database, scaffold, and real CLI integration tests remain part of the local `make check` gate. Renovate inherits the shared dependency policy; preset updates and pins stay review-only. Auto-merge must remain gated by the required `ci` check on `main`.

To start your own:

```bash
cargo build --release --package almanac-cli
target/release/almanac new my-almanac
cd my-almanac
../target/release/almanac generate workflow watch-things
```

## Documents

| Document | What it settles |
|---|---|
| [docs/vision.md](docs/vision.md) | Why this exists, and what it is not |
| [docs/core-principles.md](docs/core-principles.md) | The decision filters |
| [docs/prd.md](docs/prd.md) | The requirements, numbered, and the stack |
| [docs/roadmap.md](docs/roadmap.md) | What is deferred, and to which milestone |
| [docs/deploying.md](docs/deploying.md) | How to run it, stage by stage |
| [docs/coverage.md](docs/coverage.md) | Which test proves each requirement, and what is not met |
| [AGENTS.md](AGENTS.md) | How to work in this repository |

## License

[ISC](LICENSE.md)

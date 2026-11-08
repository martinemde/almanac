# Container build decision

Containerfiles use only first-party upstream images or official distribution images, never arbitrary third-party images. The application build uses the Rust Docker Official Image (the official language image) and the Debian official distribution image.

The root `Containerfile` builds the broker and the Renovate almanac, not a repair sandbox. It uses both Cargo lockfiles and the same Rust release as `mise.toml` and `rust-toolchain.toml`; base images are pinned by registry digest. This gives CI a repository-owned application artifact without introducing a live deployment.

The image defaults to `ENTRYPOINT ["almanac-renovate"]` and `CMD ["report"]`, with `ALMANAC_READ_ONLY=1` and `ALMANAC_MODE=dry`. The report verb independently refuses writes. The image has no Claude CLI, Podman, repair toolchains, sandbox image, or dependency caches, so it is not a full repair runtime. Mounting a container-engine socket would grant broad host control and is deliberately not part of this proof.

CI builds Linux amd64 on pull requests and main, tests the default report against an empty local fake forge and runs offline `synth`. It publishes the exact tested image on main as `ghcr.io/martinemde/almanac:main` and `sha-<commit>`, with an immutable digest in the job summary and artifact. This proves packaging and read-only startup, not real-fleet reporting, database connectivity, or repair isolation. The existing `make check` gate remains required before publishing source changes.

Renovate discovers Cargo, mise, Rust toolchain files, GitHub Actions, and `Containerfile` through native managers. Rust toolchain and Rust image updates are grouped so their versions can be reviewed together. The shared policy is pinned to its release and updates remain review-only here. Fork processing is enabled because this repository is a fork. GitHub Actions must be enabled in the fork's repository settings before the workflow can run; the first GHCR package may also need its visibility set to public.

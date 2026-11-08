FROM docker.io/library/rust:1.99.0-trixie@sha256:81015672d6939b0265f09adf96d5de125482bb1330106076956b0518e74edc90 AS build
WORKDIR /src
ENV SQLX_OFFLINE=true
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
COPY crates crates
COPY xtask xtask
COPY docs/errors docs/errors
COPY almanacs/renovate almanacs/renovate
RUN cargo build --release --locked --package broker \
    && cargo build --release --locked --manifest-path almanacs/renovate/Cargo.toml

FROM docker.io/library/debian:trixie-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2
# The broker and CLI use Rust TLS; retain the system roots for HTTPS clients.
COPY --from=build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=build /src/target/release/almanac-broker /usr/local/bin/almanac-broker
COPY --from=build /src/almanacs/renovate/target/release/almanac-renovate /usr/local/bin/almanac-renovate
ENV ALMANAC_READ_ONLY=1 ALMANAC_MODE=dry
USER 65532:65532
WORKDIR /tmp
ENTRYPOINT ["almanac-renovate"]
CMD ["report"]

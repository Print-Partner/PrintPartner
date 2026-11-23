FROM node:22.23.3-bookworm@sha256:69a5713e7f4997e96d579aafd78734810e0dac0dd5ee4621d72c27a2891a4088 AS parity-node
FROM node:24.21.0-bookworm@sha256:3d27e5c11e5786e309ec3e03f93ae536eb36e6e5eb3714d5eb3300a36157add0 AS runtime-node
FROM rust:1.100.0-bookworm@sha256:ade39a710ef85f3506cffcfec3e8a24ba090badef1dff2fb798f71d878833fea

RUN apt-get update && apt-get install -y --no-install-recommends \
    python3 python3-venv pkg-config libssl-dev \
    libglib2.0-dev libgtk-3-dev libwebkit2gtk-4.1-dev libsoup-3.0-dev \
    libayatana-appindicator3-dev librsvg2-dev mold chromium \
    && rm -rf /var/lib/apt/lists/*

RUN python3 -m venv /opt/sidecar-venv \
    && /opt/sidecar-venv/bin/pip install --no-cache-dir flask==3.1.3 waitress==3.0.2

COPY --from=parity-node /usr/local/ /opt/node22/
COPY --from=runtime-node /usr/local/ /opt/node24/
ENV PATH="/opt/node24/bin:${PATH}" \
    PP_SCHEMA_PARITY_NODE="/opt/node22/bin/node" \
    PLAYWRIGHT_CHROMIUM_EXECUTABLE="/usr/bin/chromium" \
    INSTA_UPDATE="no"

# Some existing runtime proofs deliberately use this absolute Node path.
RUN ln -s /opt/node24/bin/node /usr/bin/node \
    && rustup component add rustfmt clippy \
    && useradd --create-home --uid 1000 --shell /bin/bash developer \
    && mkdir -p /workspaces/PrintPartner \
    && chown -R developer:developer /usr/local/cargo /usr/local/rustup /workspaces

USER developer
WORKDIR /workspaces/PrintPartner
CMD ["sleep", "infinity"]

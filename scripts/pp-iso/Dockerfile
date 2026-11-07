# Image for one pp-iso job. The container is created for a single job and deleted after it,
# so nothing in it is reused. It holds no credential: the host passes a one-job JIT config
# in the environment when it starts the container.
FROM ubuntu:24.04@sha256:fe81560990fae86dc4743202b2cb4d3df98d0bcf949173cecef15ce1127ef852

ARG RUNNER_VERSION=2.338.2
ARG RUNNER_SHA256=baebf204307bac6193480852732f0aba2ae97b6c5597c244f4d5558390147c40

ENV DEBIAN_FRONTEND=noninteractive \
    LANG=C.UTF-8 \
    RUNNER_ALLOW_RUNASROOT=0 \
    ACTIONS_RUNNER_PRINT_LOG_TO_STDOUT=1

# Tools that hosted ubuntu-latest jobs of this repository use outside setup actions.
# Node, Rust and Python toolchains come from the setup actions, as on hosted runners.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      build-essential ca-certificates curl git gnupg jq libssl-dev pkg-config \
      python3 python3-pip python3-venv sudo unzip xz-utils zip \
 && install -d -m 0755 /etc/apt/keyrings \
 && curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg -o /etc/apt/keyrings/githubcli-archive-keyring.gpg \
 && echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" > /etc/apt/sources.list.d/github-cli.list \
 && apt-get update \
 && apt-get install -y --no-install-recommends gh \
 && rm -rf /var/lib/apt/lists/*

# The runner user is not root. sudo exists because some workflow steps call `sudo apt-get`;
# it is root only inside the user namespace of the rootless container, which has no host mounts.
RUN userdel -r ubuntu 2>/dev/null || true \
 && useradd --create-home --uid 1001 --shell /bin/bash runner \
 && echo 'runner ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/runner \
 && chmod 0440 /etc/sudoers.d/runner

WORKDIR /home/runner/actions-runner
RUN curl -fsSL -o runner.tar.gz "https://github.com/actions/runner/releases/download/v${RUNNER_VERSION}/actions-runner-linux-x64-${RUNNER_VERSION}.tar.gz" \
 && echo "${RUNNER_SHA256}  runner.tar.gz" | sha256sum -c - \
 && tar xzf runner.tar.gz \
 && rm runner.tar.gz \
 && ./bin/installdependencies.sh \
 && rm -rf /var/lib/apt/lists/* \
 && chown -R runner:runner /home/runner

USER runner
ENV HOME=/home/runner
# ACTIONS_RUNNER_INPUT_JITCONFIG arrives in the environment; the runner takes one job and exits.
ENTRYPOINT ["/home/runner/actions-runner/run.sh"]

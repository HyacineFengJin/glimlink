# SPDX-License-Identifier: MIT
# Build context: the extracted Linux release bundle, never the private workspace.
FROM debian:trixie-slim@sha256:f6b08b09a05725f9431502d3b5714f5c4576e82c1a84fa1debe0c24a7e0c3ec4
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY bin/agent-runtime /usr/local/bin/agent-runtime
COPY share/glimlink /usr/local/share/glimlink
COPY LICENSE NOTICE DISTRIBUTION.txt COPYING.* /usr/local/share/glimlink/
ENV HOME=/state/home \
    GLIMLINK_CODEX_BINARY=/engines/codex-native \
    GLIMLINK_ANTIGRAVITY_BINARY=/engines/antigravity-native
WORKDIR /state
USER 10001:10001
ENTRYPOINT ["/usr/local/bin/agent-runtime"]
CMD ["--state", "/state", "agent", "run", "--poll-ms", "5000", "--timeout-seconds", "360"]

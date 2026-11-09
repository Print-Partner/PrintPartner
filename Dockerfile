# Print Partner web — self-host (API + SPA on one port)
# Multi-stage build with security hardening

FROM node:22-bookworm-slim AS build
WORKDIR /app/web
RUN apt-get update && apt-get install -y --no-install-recommends python3 make g++ && \
    rm -rf /var/lib/apt/lists/*
COPY web/package.json web/package-lock.json ./
COPY web/apps/web/package.json ./apps/web/
COPY web/apps/server/package.json ./apps/server/
COPY web/packages/contracts/package.json ./packages/contracts/
COPY web/packages/domain/package.json ./packages/domain/
RUN npm ci
COPY web/ ./
# Vite inlines VITE_* at build time. An empty DSN leaves the browser SDK off.
ARG VITE_SENTRY_DSN=
ENV VITE_SENTRY_DSN=$VITE_SENTRY_DSN
RUN npm run build && \
    # Remove dev dependencies to reduce final image size
    npm ci --omit=dev

FROM node:22-bookworm-slim AS runtime
# Release builds override these with the validated, peeled tag identity.
ARG PP_APP_VERSION=3.3.0
ARG PP_COMMIT=
ARG PP_TAG=
ARG PP_BUILD_DATE=unknown

LABEL org.opencontainers.image.source="https://github.com/Print-Partner/PrintPartner" \
      org.opencontainers.image.version="${PP_APP_VERSION}" \
      org.opencontainers.image.revision="${PP_COMMIT}" \
      org.opencontainers.image.ref.name="${PP_TAG}" \
      org.opencontainers.image.created="${PP_BUILD_DATE}"

# dumb-init: proper PID 1 signal handling. gosu: drop root after entrypoint chown.
RUN apt-get update && apt-get install -y --no-install-recommends dumb-init gosu && \
    rm -rf /var/lib/apt/lists/*

# Create non-root user for application (uid 1000, gid 1000).
# node:bookworm-slim already has uid/gid 1000 as "node" — reclaim for ppuser.
RUN if ! id ppuser >/dev/null 2>&1; then \
      if id node >/dev/null 2>&1; then \
        groupmod -n ppuser node && \
        usermod -l ppuser -d /home/ppuser -m -s /sbin/nologin -c "Print Partner user" node; \
      else \
        groupadd -g 1000 ppuser && \
        useradd -u 1000 -g ppuser -d /home/ppuser -s /sbin/nologin -c "Print Partner user" ppuser; \
      fi; \
    fi && \
    mkdir -p /home/ppuser && \
    chown -R ppuser:ppuser /home/ppuser

WORKDIR /app/web
ENV NODE_ENV=production
ENV PP_VERSION=${PP_APP_VERSION}-web
ENV PP_COMMIT=${PP_COMMIT}
ENV PP_TAG=${PP_TAG}
ENV PP_BUILD_DATE=${PP_BUILD_DATE}
# Listen on every container interface so `docker run -p 8080:8080` and Compose
# can publish the app. Compose also publishes the host port (see PP_BIND_ADDRESS).
ENV HOST=0.0.0.0
ENV PORT=8080
ENV PRINT_PARTNER_DATA_DIR=/data
ENV STATIC_DIR=/app/web/apps/web/dist

# Copy built application with correct ownership
COPY --from=build --chown=ppuser:ppuser /app/web ./

WORKDIR /app/web/apps/server

# Image-layer /data ownership (named volume mounts hide this; entrypoint fixes at runtime)
RUN mkdir -p /data && chown -R ppuser:ppuser /data

COPY docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN chmod +x /usr/local/bin/docker-entrypoint.sh

EXPOSE 8080

# Starts as root so entrypoint can chown named-volume /data, then drops to ppuser.
# Do not set USER here — compose must not force user: "1000:1000" either.
ENTRYPOINT ["docker-entrypoint.sh"]
CMD ["node", "dist/current/index.js"]

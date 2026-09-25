# syntax=docker/dockerfile:1
# Build context: repository root. The image carries only the edge: theme and checkout artifacts
# are published by `make theme-build` and downloaded from the platform on first use (spec A22).
# Debian, not Alpine: the workerd binary needs glibc.
FROM node:24.21.0-bookworm-slim
WORKDIR /repo
RUN corepack enable
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY packages/config/package.json packages/config/
COPY packages/theme-kit/package.json packages/theme-kit/
COPY apps/edge/package.json apps/edge/
RUN pnpm install --frozen-lockfile --filter @platform/edge...
COPY packages/config packages/config
COPY packages/theme-kit packages/theme-kit
COPY apps/edge apps/edge
# Downloaded, verified artifacts (a volume in compose).
RUN mkdir -p /var/cache/artifacts && chown node:node /var/cache/artifacts
ENV NODE_ENV=production ARTIFACT_ROOT=/var/cache/artifacts PORT=8787 ADMIN_PORT=8788
USER node
EXPOSE 8787 8788
CMD ["node", "apps/edge/src/server.ts"]

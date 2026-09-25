# syntax=docker/dockerfile:1
# Build context: repository root. The image carries the edge plus the default-theme and checkout
# artifacts, built and packed at image build time (spec A22/A30: one shared default artifact).
# Debian, not Alpine: the workerd binary needs glibc.
FROM node:24.21.0-bookworm-slim
WORKDIR /repo
RUN corepack enable
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY packages/config/package.json packages/config/
COPY packages/theme-kit/package.json packages/theme-kit/
COPY packages/storefront-sdk/package.json packages/storefront-sdk/
COPY apps/edge/package.json apps/edge/
COPY apps/checkout/package.json apps/checkout/
COPY themes/default/package.json themes/default/
RUN pnpm install --frozen-lockfile \
  --filter @platform/edge... --filter @platform/theme-default... --filter @platform/checkout...
COPY packages/config packages/config
COPY packages/theme-kit packages/theme-kit
COPY packages/storefront-sdk packages/storefront-sdk
COPY apps/edge apps/edge
COPY apps/checkout apps/checkout
COPY themes/default themes/default
COPY scripts/build-artifacts.sh scripts/
# Root-owned and read-only for the runtime user.
RUN ARTIFACT_ROOT=/artifacts scripts/build-artifacts.sh
ENV NODE_ENV=production ARTIFACT_ROOT=/artifacts SITES_FILE=/repo/apps/edge/sites.local.json \
  PORT=8787 ADMIN_PORT=8788
USER node
EXPOSE 8787 8788
CMD ["node", "apps/edge/src/server.ts"]

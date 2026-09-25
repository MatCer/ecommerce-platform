# syntax=docker/dockerfile:1
# Build context: repository root (pnpm workspace). Node 24 runs the TypeScript directly
# (type stripping), so there is no build step.
FROM node:24.21.0-alpine
WORKDIR /repo
RUN corepack enable
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY packages/config/package.json packages/config/
COPY apps/mocks/package.json apps/mocks/
RUN pnpm install --frozen-lockfile --prod --filter @platform/mocks
COPY apps/mocks/src apps/mocks/src
# Fixture product photos for the stub Storefront API (WP2, until WP3/WP6 serve real media).
COPY fixtures/media fixtures/media
ENV NODE_ENV=production PORT=4010 MEDIA_DIR=/repo/fixtures/media
USER node
EXPOSE 4010
CMD ["node", "apps/mocks/src/server.ts"]

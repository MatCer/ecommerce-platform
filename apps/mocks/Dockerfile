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
ENV NODE_ENV=production PORT=4010
USER node
EXPOSE 4010
CMD ["node", "apps/mocks/src/server.ts"]

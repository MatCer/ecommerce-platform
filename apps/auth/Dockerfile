# syntax=docker/dockerfile:1
# Better Auth service (spec D11). Build context: repository root (pnpm workspace). Node 24 runs
# the TypeScript directly (type stripping), so there is no build step.
FROM node:24.21.0-alpine
WORKDIR /repo
RUN corepack enable
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY packages/config/package.json packages/config/
COPY apps/auth/package.json apps/auth/
RUN pnpm install --frozen-lockfile --prod --filter @platform/auth
COPY apps/auth/src apps/auth/src
ENV NODE_ENV=production PORT=3000
USER node
EXPOSE 3000
CMD ["node", "apps/auth/src/server.ts"]

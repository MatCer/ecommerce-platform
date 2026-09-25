# syntax=docker/dockerfile:1
# Admin SPA (spec D10): a static Vite build served by Caddy. Build context: repository root.
FROM node:24.21.0-alpine AS build
WORKDIR /repo
RUN corepack enable
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY packages/config/package.json packages/config/
COPY packages/ui/package.json packages/ui/
COPY packages/admin-client/package.json packages/admin-client/
COPY apps/admin/package.json apps/admin/
RUN pnpm install --frozen-lockfile --filter "@platform/admin..."
COPY packages/config packages/config
COPY packages/ui/src packages/ui/src
COPY packages/admin-client/src packages/admin-client/src
COPY apps/admin apps/admin
RUN pnpm --filter @platform/admin build

FROM caddy:2.11.4-alpine
COPY apps/admin/Caddyfile /etc/caddy/Caddyfile
COPY --from=build /repo/apps/admin/dist /srv
EXPOSE 80

# syntax=docker/dockerfile:1
# Theme builder + sandbox image (WP23, spec §12.3, A6). One image, three roles:
#   theme-builder service      node apps/theme-builder/src/server.ts   (no socket, no secrets)
#   theme-sandbox-proxy        node apps/theme-builder/src/proxy.ts    (holds the Docker socket)
#   sandbox steps (disposable) node apps/theme-builder/src/sandbox.ts <step>
#                              run --read-only --network none|internal, non-root, tmpfs /work
# The locked theme dependencies (themes/default/node_modules) and Playwright's Chromium live
# in the image, so the read-only root filesystem is the read-only node_modules of A6.
# Debian, not Alpine: Chromium and workerd need glibc. Build context: repository root.
FROM node:24.21.0-bookworm-slim
WORKDIR /repo
RUN corepack enable
ENV PLAYWRIGHT_BROWSERS_PATH=/ms-playwright
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY packages/config/package.json packages/config/
COPY packages/theme-kit/package.json packages/theme-kit/
COPY packages/storefront-sdk/package.json packages/storefront-sdk/
COPY themes/default/package.json themes/default/
COPY apps/theme-builder/package.json apps/theme-builder/
RUN pnpm install --frozen-lockfile --filter "@platform/theme-builder..." \
    && pnpm --filter @platform/theme-builder exec playwright install --with-deps chromium \
    && rm -rf /root/.cache /root/.local/share/pnpm /var/lib/apt/lists/*
COPY packages/config packages/config
COPY packages/theme-kit packages/theme-kit
COPY packages/storefront-sdk packages/storefront-sdk
COPY themes/default themes/default
COPY apps/theme-builder apps/theme-builder
# /work: the job volume's mount point in the builder (the volume inherits this owner).
RUN mkdir -p /work && chown node:node /work
# No telemetry calls from build tools (the build steps have no network anyway).
ENV NODE_ENV=production PORT=4020 WORK_DIR=/work ASTRO_TELEMETRY_DISABLED=1
USER node
EXPOSE 4020
CMD ["node", "apps/theme-builder/src/server.ts"]

FROM node:24.14.0-alpine

ENV NODE_ENV=production \
    HOST=0.0.0.0 \
    PORT=18080

WORKDIR /app

RUN mkdir -p /data/attachments && chown -R 1000:1000 /data/attachments

COPY package.json package-lock.json ./
RUN npm ci --omit=dev

COPY --chown=node:node src ./src

USER node

EXPOSE 18080

HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
  CMD wget -q -O - http://127.0.0.1:18080/healthz >/dev/null || exit 1

CMD ["node", "src/index.js"]

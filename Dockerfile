FROM node:24-alpine AS frontend-builder

COPY frontend /frontend

WORKDIR /frontend

RUN npm ci
RUN npm run build


FROM rust:1.94-alpine as server-builder


COPY server /server

WORKDIR /server

RUN apk add --no-cache musl-dev sqlite sqlite-dev sqlite-static pkgconfig

RUN cargo build --release

FROM debian:13 

ENV DEBIAN_FRONTEND=noninteractive

RUN useradd -ms /bin/bash user -u 1000

RUN apt-get update && apt-get install -y ffmpeg yt-dlp && apt-get clean

RUN mkdir -p /app/frontend
USER user 
COPY --chown=1000:1000 --from=server-builder /server/target/release/server /app/server

COPY --chown=1000:1000 --from=frontend-builder /frontend/build/. /app/frontend

ENV FRONTEND_FOLDER=/app/frontend

EXPOSE 3000

CMD [ "/app/server" ]
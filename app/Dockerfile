# Linux build/test box for the Tauri app (keeps WebKit/GTK dev packages off the host).
#   docker build -t sorapatch-build app        (from the installer workspace root)
#   then run cargo / cargo tauri build inside it with the workspace mounted (README.md "Build").
# Ubuntu 22.04 (glibc 2.35): an AppImage built here also runs on newer distros (Tauri's advice);
# 22.04 ships webkit2gtk-4.1 in jammy-updates.
FROM ubuntu:22.04
# Docker storage is on a spinning disk (/data): dpkg's per-file fsync made this step take 2+ hours. Skip it.
RUN echo force-unsafe-io > /etc/dpkg/dpkg.cfg.d/unsafe-io
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      build-essential pkg-config ca-certificates curl wget file patchelf \
      libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev librsvg2-dev libxdo-dev libssl-dev \
      xvfb xauth x11-apps imagemagick dbus-x11 fonts-noto-core \
 && rm -rf /var/lib/apt/lists/*

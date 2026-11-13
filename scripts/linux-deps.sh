#!/usr/bin/env bash
# Installs the system libraries GPUI's Wayland and X11 backends build against
# on Debian and Ubuntu.
set -euo pipefail

sudo apt-get update
sudo apt-get install -y --no-install-recommends \
  clang \
  libfontconfig-dev \
  libfreetype-dev \
  libglib2.0-dev \
  libssl-dev \
  libvulkan1 \
  libwayland-dev \
  libx11-xcb-dev \
  libxcb1-dev \
  libxkbcommon-dev \
  libxkbcommon-x11-dev \
  libzstd-dev

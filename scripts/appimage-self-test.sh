#!/bin/sh
# Runs the packaged AppImage's self-test: git, gh and xdg-open must start as the
# app starts them, and git must reach a public repository over HTTPS (which
# loads libcurl). Usage: scripts/appimage-self-test.sh path/to/App.AppImage
set -eu
appimage=$1
chmod +x "$appimage"
# Runners have no FUSE: the runtime extracts the image and runs it from there,
# with the same AppRun and environment as a mounted image.
APPIMAGE_EXTRACT_AND_RUN=1 "$appimage" --self-test https://github.com/octocat/Hello-World

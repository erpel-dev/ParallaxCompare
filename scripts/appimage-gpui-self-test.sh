#!/bin/sh
# Runs the GPUI AppImage's self-test: git, gh and xdg-open must start as the app
# starts them, git must reach a public repository over HTTPS, and a window must
# open and draw (Vulkan, X11 and fonts from the system). Without a display it
# starts Xvfb with a window manager, as GPUI draws only once a window manager
# reports the window visible. Needs xvfb, openbox and a Vulkan driver (Mesa's
# lavapipe on machines without a GPU).
# Usage: scripts/appimage-gpui-self-test.sh path/to/App.AppImage
set -eu
appimage=$1
chmod +x "$appimage"
cleanup=""
if [ -z "${DISPLAY:-}" ]; then
  export DISPLAY=:97
  Xvfb "$DISPLAY" -screen 0 1280x800x24 >/dev/null 2>&1 &
  cleanup="$!"
  sleep 1
  openbox >/dev/null 2>&1 &
  cleanup="$cleanup $!"
  sleep 1
fi
status=0
# Runners have no FUSE: the runtime extracts the image and runs it from there.
APPIMAGE_EXTRACT_AND_RUN=1 "$appimage" --self-test https://github.com/octocat/Hello-World || status=$?
[ -n "$cleanup" ] && kill $cleanup 2>/dev/null
exit "$status"

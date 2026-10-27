#!/usr/bin/env bash
# Own D-Bus session, runtime socket, framebuffer and configuration; never use the user's display.
set -euo pipefail
root=$(mktemp -d /tmp/dbdeck-gui-XXXXXX)
mkdir -m 700 "$root/runtime"
export XDG_RUNTIME_DIR="$root/runtime" XDG_CONFIG_HOME="$root/config" XDG_DATA_HOME="$root/data" XDG_CACHE_HOME="$root/cache"
unset DISPLAY WAYLAND_DISPLAY SESSION_MANAGER
export QT_QPA_PLATFORM=offscreen LIBGL_ALWAYS_SOFTWARE=1 QT_QUICK_BACKEND=software
export GTK_A11Y=none GIO_USE_VFS=local GSETTINGS_BACKEND=memory
export DBDECK_GUI_TEST_ROOT="$root"
exec dbus-run-session -- bash -c '
set -euo pipefail
kwin_wayland --virtual --width 1600 --height 1000 --no-lockscreen --no-global-shortcuts --no-kactivities --socket dbdeck-test >"$DBDECK_GUI_TEST_ROOT/compositor.log" 2>&1 &
compositor_pid=$!
trap "kill $compositor_pid 2>/dev/null || true" EXIT
for ((attempt=0;attempt<100;attempt++)); do
    if [[ -S "$XDG_RUNTIME_DIR/dbdeck-test" ]]; then break; fi
    if ! kill -0 "$compositor_pid" 2>/dev/null; then cat "$DBDECK_GUI_TEST_ROOT/compositor.log"; exit 1; fi
    sleep 0.1
done
if [[ ! -S "$XDG_RUNTIME_DIR/dbdeck-test" ]]; then cat "$DBDECK_GUI_TEST_ROOT/compositor.log"; exit 1; fi
export WAYLAND_DISPLAY=dbdeck-test GDK_BACKEND=wayland
if command -v dbus-update-activation-environment >/dev/null; then
    dbus-update-activation-environment WAYLAND_DISPLAY XDG_RUNTIME_DIR XDG_CONFIG_HOME XDG_DATA_HOME XDG_CACHE_HOME GTK_A11Y
fi
"$@"
' dbdeck-headless "$@"

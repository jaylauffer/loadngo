#!/bin/sh
# Installs loadngo's system_monitor as a desktop widget for the current user:
# top-right corner, no title bar, kept below other windows, on every
# workspace, started at login. On a labwc desktop (Raspberry Pi OS) or macOS.
#
#   scripts/install-system-monitor.sh target/release/system_monitor
#
# Both install the binary as ~/.local/bin/loadngo-system-monitor and start it.
# labwc: adds a window rule for app_id "loadngo-system-monitor" to
# ~/.config/labwc/rc.xml (backing up an existing file first; labwc must run
# with -m, as Raspberry Pi OS starts it), adds
# ~/.config/autostart/loadngo-system-monitor.desktop and reloads labwc.
# macOS: the monitor places its own window; adds the LaunchAgent
# ~/Library/LaunchAgents/com.loadngo.system-monitor.plist (starts at login).
# Running it again replaces the binary and restarts the monitor.
#
# Undo (labwc): rm ~/.local/bin/loadngo-system-monitor
#   ~/.config/autostart/loadngo-system-monitor.desktop, delete the
#   loadngo-system-monitor windowRule from ~/.config/labwc/rc.xml.
# Undo (macOS): launchctl bootout gui/$(id -u)/com.loadngo.system-monitor; rm
#   ~/Library/LaunchAgents/com.loadngo.system-monitor.plist
#   ~/.local/bin/loadngo-system-monitor
set -eu

if [ $# -ne 1 ] || [ "$1" = "--help" ] || [ "$1" = "-h" ]; then
    sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//'
    [ $# -eq 1 ] && exit 0 || exit 2
fi
binary=$1
[ -x "$binary" ] || { echo "not an executable: $binary" >&2; exit 1; }

app_id=loadngo-system-monitor
installed="$HOME/.local/bin/$app_id"
mkdir -p "$HOME/.local/bin" "$HOME/.config/labwc" "$HOME/.config/autostart"

# Stop a running copy before replacing the file it runs from.
pkill -f "^$installed" 2>/dev/null || true
install -m 755 "$binary" "$installed"

if [ "$(uname -s)" = Darwin ]; then
    label=com.loadngo.system-monitor
    agent="$HOME/Library/LaunchAgents/$label.plist"
    mkdir -p "$HOME/Library/LaunchAgents"
    cat > "$agent" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$label</string>
  <key>ProgramArguments</key><array><string>$installed</string></array>
  <key>RunAtLoad</key><true/>
  <key>LimitLoadToSessionType</key><string>Aqua</string>
  <key>ProcessType</key><string>Interactive</string>
</dict>
</plist>
EOF
    domain="gui/$(id -u)"
    launchctl bootout "$domain/$label" 2>/dev/null || true
    launchctl bootstrap "$domain" "$agent"
    echo "installed; LaunchAgent $label started it and starts it at login"
    exit 0
fi

rule="<windowRule identifier=\"$app_id\" serverDecoration=\"no\" skipTaskbar=\"yes\" skipWindowSwitcher=\"yes\"><action name=\"MoveToEdge\" direction=\"right\" snapWindows=\"no\" /><action name=\"MoveToEdge\" direction=\"up\" snapWindows=\"no\" /><action name=\"ToggleAlwaysOnBottom\" /><action name=\"ToggleOmnipresent\" /></windowRule>"
rc="$HOME/.config/labwc/rc.xml"
if [ ! -f "$rc" ]; then
    printf '<?xml version="1.0"?>\n<openbox_config xmlns="http://openbox.org/3.4/rc"><windowRules>%s</windowRules></openbox_config>\n' "$rule" > "$rc"
elif ! grep -q "identifier=\"$app_id\"" "$rc"; then
    cp "$rc" "$rc.before-$app_id"
    if grep -q '<windowRules>' "$rc"; then
        sed -i "s#<windowRules>#<windowRules>$rule#" "$rc"
    else
        sed -i "s#</openbox_config>#<windowRules>$rule</windowRules></openbox_config>#" "$rc"
    fi
fi

cat > "$HOME/.config/autostart/$app_id.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=System monitor
Comment=loadngo system monitor: CPU, thermal pressure, memory and disk
Exec=$installed
Terminal=false
EOF

labwc_pid=$(pgrep -u "$(id -u)" -x labwc || true)
if [ -z "$labwc_pid" ]; then
    echo "installed; labwc is not running, so the monitor starts at the next login"
    exit 0
fi
kill -HUP "$labwc_pid"
runtime_dir=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}
display=${WAYLAND_DISPLAY:-$(basename "$(ls "$runtime_dir"/wayland-[0-9] 2>/dev/null | head -n 1)")}
XDG_RUNTIME_DIR=$runtime_dir WAYLAND_DISPLAY=$display setsid -f "$installed" >/dev/null 2>&1 < /dev/null
echo "installed and started on $display"

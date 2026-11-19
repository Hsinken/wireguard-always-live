#!/usr/bin/env bash
# Guided install or upgrade of WG Always Live (WGAL) on Ubuntu for people:
#
#   ./setup.sh             # asks for your sudo password once
#   ./setup.sh --dry-run   # shows the same choices, runs nothing, no sudo
#
# It lists this machine's wg-quick tunnels (install.sh --list-tunnels),
# lets you tick the ones WGAL may monitor in a check list (whiptail, as
# Ubuntu ships it; a numbered list if it is missing), and runs this
# folder's install.sh with them and with the account that started it as
# the WGAL window's user. On an upgrade it first asks whether to keep the
# current access list, which install.sh then keeps (no --tunnel).
#
# On a computer without a graphical desktop (graphical.target not active
# and no DISPLAY or WAYLAND_DISPLAY), such as a server, it asks whether to
# install without the window (the default) and then passes --no-window:
# WGAL is used with its command line, wgal (owner decision of 2026-11-18).
# Otherwise, when the Ubuntu packages the WGAL window needs are missing
# (libwebkit2gtk-4.1-0, libayatana-appindicator3-1), it asks whether
# install.sh may install them with apt-get, and passes --install-runtime
# when the user agrees; without them the window cannot open.
#
# It changes nothing itself, never reads a WireGuard configuration's
# contents and never starts, stops or changes a tunnel: install.sh does the
# work and says what it did. It speaks Traditional Chinese when the
# locale is Chinese and English otherwise.
#
# Exit codes: install.sh's; 10 cancelled or nothing to do.
set -euo pipefail
PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH

here=$(cd "$(dirname "$0")" && pwd -P)
readonly ACCESS_FILE=/etc/wgal/access.conf
readonly UNIT_FILE=/etc/systemd/system/wgal-watchdog.service

dry_run=0
case "${1:-}" in
--dry-run) dry_run=1 ;;
"") ;;
*)
    echo "usage: ./setup.sh [--dry-run]" >&2
    exit 64
    ;;
esac

zh=0
case "${LC_ALL:-${LC_MESSAGES:-${LANG:-}}}" in zh*) zh=1 ;; esac
t() { if [ "$zh" -eq 1 ]; then printf '%s' "$1"; else printf '%s' "$2"; fi; }

# --- Root once: start again with sudo, passing on who started it ------------
if [ "$dry_run" -eq 0 ] && [ "$(id -u)" -ne 0 ]; then
    t "安裝 WGAL 需要系統管理員權限，sudo 會詢問一次密碼。" \
        "Installing WGAL needs root; sudo asks for your password once."
    echo
    exec sudo -- "$here/setup.sh"
fi
user=${SUDO_USER:-$(id -un)}
if [ "$dry_run" -eq 0 ] && { [ -z "${SUDO_USER:-}" ] || [ "$user" = root ]; }; then
    t "請從你自己的帳號用 ./setup.sh 執行（它會用 sudo），WGAL 視窗不能以 root 執行。" \
        "Run ./setup.sh from your own account (it uses sudo); the WGAL window must not run as root."
    echo
    exit 10
fi
[ -x "$here/install.sh" ] || {
    t "這個資料夾裡沒有 install.sh。請從解壓縮後的發布包資料夾執行。" \
        "install.sh is not in this folder. Run this from the unpacked release folder."
    echo
    exit 10
}

# --- The tunnels and the current list ------------------------------------------
listed=$("$here/install.sh" --list-tunnels)
names=() states=() rejected=()
complete=false
[[ "$listed" == '{"complete":true'* ]] && complete=true
re='^\{"name":"([^"]+)","status":"([a-z]+)","starts_at_boot":(true|false),"accepted":(true|false)\}'
while IFS= read -r line; do
    [[ "$line" =~ $re ]] || continue
    if [ "${BASH_REMATCH[4]}" = true ]; then
        names+=("${BASH_REMATCH[1]}")
        state=$(if [ "${BASH_REMATCH[2]}" = running ]; then t "執行中" "running"; else t "已停止" "stopped"; fi)
        [ "${BASH_REMATCH[3]}" = true ] && state="$state$(t "，開機自動啟動" ", starts at boot")"
        states+=("$state")
    else
        rejected+=("${BASH_REMATCH[1]}")
    fi
done <<<"$listed"

upgrade=0
[ -e "$UNIT_FILE" ] && upgrade=1
current=()
if [ -f "$ACCESS_FILE" ] && [ ! -L "$ACCESS_FILE" ] && [ -r "$ACCESS_FILE" ]; then
    while read -r key value _; do
        [ "$key" = tunnel ] && [ -n "$value" ] && current+=("$value")
    done <"$ACCESS_FILE"
fi

if [ "${#names[@]}" -eq 0 ] && [ "$upgrade" -eq 0 ]; then
    t "找不到 WireGuard tunnel（/etc/wireguard/<名稱>.conf）。請先加入要監看的 tunnel，再執行一次。" \
        "No WireGuard tunnel (/etc/wireguard/<name>.conf) was found. Add the tunnels to monitor, then run this again."
    echo
    exit 10
fi
if [ "$complete" = false ]; then
    t "（沒有 root 權限，只列出 systemd 知道的與正在執行的 tunnel。）" \
        "(Without root, only the tunnels systemd knows and the running ones are listed.)"
    echo
fi
if [ "${#rejected[@]}" -gt 0 ]; then
    echo "$(t "WGAL 不接受這些名稱，不能選：" "WGAL does not accept these names, so they cannot be picked: ")${rejected[*]}"
fi

joined() { local IFS=,; printf '%s' "$*"; }
in_current() {
    local c
    for c in "${current[@]}"; do [ "$c" = "$1" ] && return 0; done
    return 1
}

# --- Choose ----------------------------------------------------------------
keep=0
chosen=()
title="WGAL $(t "安裝" "setup")"
if [ "$upgrade" -eq 1 ]; then
    list=$(if [ "${#current[@]}" -gt 0 ]; then joined "${current[@]}"; else t "（讀不到）" "(not readable)"; fi)
    question="$(t "WGAL 已經安裝，這次會升級。要維持目前允許的 tunnel 嗎？" "WGAL is installed; this upgrades it. Keep the tunnels it may use now?")
$list"
    if [ -x /usr/bin/whiptail ]; then
        if whiptail --title "$title" --yes-button "$(t "維持" "Keep")" --no-button "$(t "重新選擇" "Choose again")" \
            --yesno "$question" 12 70; then
            keep=1
        fi
    else
        echo "$question"
        read -r -p "$(t "維持？[Y/n] " "Keep? [Y/n] ")" answer </dev/tty || answer=n
        case "$answer" in [nN]*) ;; *) keep=1 ;; esac
    fi
fi

if [ "$keep" -eq 0 ]; then
    if [ "${#names[@]}" -eq 0 ]; then
        t "沒有可以選的 tunnel，沒有變更任何東西。" "There is no tunnel to pick; nothing was changed."
        echo
        exit 10
    fi
    prompt=$(t "勾選要交給 WGAL 監看的 tunnel（空白鍵勾選，Enter 確認）：" "Tick the tunnels WGAL may monitor (space ticks, Enter confirms):")
    if [ -x /usr/bin/whiptail ]; then
        items=()
        for i in "${!names[@]}"; do
            on=OFF
            in_current "${names[$i]}" && on=ON
            items+=("${names[$i]}" "${states[$i]}" "$on")
        done
        if ! picked=$(whiptail --title "$title" --separate-output --ok-button "$(t "安裝" "Install")" \
            --cancel-button "$(t "取消" "Cancel")" --checklist "$prompt" 20 76 12 "${items[@]}" 3>&1 1>&2 2>&3); then
            t "已取消，沒有變更任何東西。" "Cancelled; nothing was changed."
            echo
            exit 10
        fi
        while IFS= read -r name; do
            [ -n "$name" ] && chosen+=("$name")
        done <<<"$picked"
    else
        t "要交給 WGAL 監看的 tunnel：" "The tunnels WGAL may monitor:"
        echo
        for i in "${!names[@]}"; do
            echo "  $((i + 1))) ${names[$i]}  (${states[$i]})"
        done
        read -r -p "$(t "輸入編號，以空白分隔（例如 1 3）：" "Numbers, separated by spaces (for example 1 3): ")" answer </dev/tty || answer=""
        for n in $answer; do
            # Decimal only: no leading zero, which bash arithmetic would
            # read as octal.
            if ! [[ "$n" =~ ^[1-9][0-9]{0,3}$ ]] || [ "$((10#$n))" -gt "${#names[@]}" ]; then
                t "編號不對，沒有變更任何東西。" "Not a listed number; nothing was changed."
                echo
                exit 10
            fi
            name=${names[$((10#$n - 1))]}
            in_list=0
            for c in "${chosen[@]}"; do [ "$c" = "$name" ] && in_list=1; done
            [ "$in_list" -eq 1 ] || chosen+=("$name")
        done
    fi
    if [ "${#chosen[@]}" -eq 0 ]; then
        t "沒有勾選任何 tunnel，沒有變更任何東西。" "No tunnel was ticked; nothing was changed."
        echo
        exit 10
    fi
fi

# --- Without a desktop: no window --------------------------------------------
no_window=()
graphical=0
systemctl is-active --quiet graphical.target 2>/dev/null && graphical=1
[ -z "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ] || graphical=1
if [ "$graphical" -eq 0 ]; then
    question="$(t "這台電腦沒有圖形桌面。要不裝視窗，改用命令列 wgal 使用 WGAL 嗎？（建議）" "This computer has no graphical desktop. Install WGAL without its window, to use it with its command line, wgal? (recommended)")"
    agreed=0
    if [ -x /usr/bin/whiptail ]; then
        whiptail --title "$title" --yes-button "$(t "不裝視窗" "No window")" --no-button "$(t "安裝視窗" "Install the window")"             --yesno "$question" 12 76 && agreed=1
    else
        echo "$question"
        # Enter alone, or end of input, is the default: no window.
        read -r -p "$(t "不裝視窗？[Y/n] " "No window? [Y/n] ")" answer </dev/tty || answer=""
        case "$answer" in [nN]*) ;; *) agreed=1 ;; esac
    fi
    [ "$agreed" -eq 0 ] || no_window=(--no-window)
fi

# --- The window's packages -------------------------------------------------
runtime=()
[ "${#no_window[@]}" -gt 0 ] ||
for package in libwebkit2gtk-4.1-0 libayatana-appindicator3-1; do
    # Installed and not in error, whether held or not.
    [ "$(dpkg-query -W -f='${db:Status-Status} ${db:Status-Eflag}' "$package" 2>/dev/null)" = "installed ok" ] ||
        runtime+=("$package")
done
install_runtime=()
if [ "${#runtime[@]}" -gt 0 ]; then
    question="$(t "WGAL 視窗需要這些 Ubuntu 套件，這台電腦還沒有：" "The WGAL window needs these Ubuntu packages, which this computer lacks:")
${runtime[*]}

$(t "要現在用 apt-get 安裝嗎？不安裝的話，視窗無法開啟。" "Install them now with apt-get? Without them the window cannot open.")"
    agreed=0
    if [ -x /usr/bin/whiptail ]; then
        whiptail --title "$title" --yes-button "$(t "安裝" "Install")" --no-button "$(t "取消" "Cancel")" \
            --yesno "$question" 14 76 && agreed=1
    else
        echo "$question"
        # End of input (Ctrl-D) is a no.
        read -r -p "$(t "安裝？[y/N] " "Install? [y/N] ")" answer </dev/tty || answer=""
        case "$answer" in [yY]*) agreed=1 ;; esac
    fi
    if [ "$agreed" -eq 0 ]; then
        t "已取消，沒有變更任何東西。" "Cancelled; nothing was changed."
        echo
        exit 10
    fi
    install_runtime=(--install-runtime)
fi

# --- Install ----------------------------------------------------------------
args=("${install_runtime[@]}" "${no_window[@]}")
if [ "$keep" -eq 1 ]; then
    t "升級，維持目前的清單。" "Upgrading; keeping the current list."
else
    args+=(--tunnel "$(joined "${chosen[@]}")" --user "$user")
    echo "$(t "安裝，允許的 tunnel：" "Installing; tunnels: ")${chosen[*]}"
fi
echo
if [ "$dry_run" -eq 1 ]; then
    echo "$(t "試跑，不會執行：" "Dry run, not run: ")sudo ./install.sh ${args[*]:-}"
    exit 0
fi
status=0
"$here/install.sh" "${args[@]}" || status=$?
echo
if [ "$status" -eq 0 ] && [ "${#no_window[@]}" -gt 0 ]; then
    t "完成。用 /opt/wgal/wgal status 查看 WGAL，/opt/wgal/wgal --help 看其他指令（install.sh 能建立連結時，也可以只打 wgal）。" "Done. \"/opt/wgal/wgal status\" shows WGAL; \"/opt/wgal/wgal --help\" the rest (or plain wgal, where install.sh could link it)."
elif [ "$status" -eq 0 ]; then
    t "完成。可以從應用程式選單開啟 WGAL。" "Done. Open WGAL from the applications menu."
else
    echo "$(t "沒有完成，代碼 " "Not completed, code ")$status$(t "。請看上面的訊息。" ". See the messages above.")"
fi
echo
exit "$status"

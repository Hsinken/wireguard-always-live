# WGAL 在 Ubuntu 上的安裝、升級與移除

[English](README.md) | **繁體中文**

安裝、驗證與移除的步驟見 `INSTALL.zh-TW.md`；這份文件說明腳本做了什麼、檔案在哪裡、出問題時怎麼看。

> **狀態（2026-11-19）**：
> - 在 WSL 的 Ubuntu 24.04 上原生建置，通過全部測試與 shellcheck；`install.sh`／`uninstall.sh` 與 systemd 服務也在 WSL 裡實際安裝、升級、移除過。
> - 2026-11-12 在一台有桌面的 Ubuntu 26.04 測試機上以 `setup.sh` 安裝 2.2.0。補裝視窗需要的套件後，視窗在桌面工作階段啟動沒有錯誤，系統匣項目有註冊；從應用程式選單點開沒有確認。之後重新安裝為 2.2.1；2026-11-13 在那台測試機上以 root 做了實機升級測試，其中一次在停止服務之後注入失敗：PASS。
> - 2026-11-19 在一台只有一條 `wg-quick` 隧道的 Ubuntu Server 26.04 上，以 `--no-window` 依序安裝 4.0.0、4.1.0 與 4.2.1，並以 4.1.0 與 4.2.1 經 WatchDog 實測：綁定介面的探測、斷線／連線／重連並確認通訊、可以開啟自動復原，WatchDog 重新啟動與重開機之後設定都保留（[docs/testing.md](../../docs/testing.md)）。
> - 這些是「已安裝到裝置」，不是「已在裝置上驗收」。
> - **還沒有做的**（NOT RUN）：4.2.1 裝在有桌面的電腦上；兩條隧道；自動復原修不好的真實故障。

## 運作方式

- **WatchDog**：`wgal-watchdog.service`，systemd 以 root 執行，開機自動啟動。它以 `systemctl start/stop wg-quick@<介面>` 控制隧道，並經由每條隧道探測。它從不修改 WireGuard 設定，也不改 `wg-quick@` 單元的開機自動啟動（`enable`／`disable`）。
- **算數的探測**：探測是綁在隧道介面上的 ping（`ping -I`），只有兩次路由查詢都指向那個介面與它的位址時才送出。4.2.0 以前 Ubuntu 不把這種探測算成經過隧道（還沒在實機上證明），所以燈號不會變綠、連線與重連會以 `control_failed` 結束、自動復原開不起來。4.2.0 起算數（2026-11-19 在一條隧道上的實機測試，[docs/testing.md](../../docs/testing.md)；升級到 4.2.1 之後燈號變綠、連線與重連都確認通訊，WatchDog 重新啟動後與重開機後設定都保留）。不在那些測試內、在 Ubuntu 上仍未證明的：兩條路由重疊的隧道，以及自動復原修不好的真實故障（警報、重試與「已恢復」通知）。隧道開機時自己沒起來的話，WGAL 不會去啟動它：它只對看過連上的隧道動作。目標只能用位址（ICMP）：TCP 探測還不能綁在隧道上。
- **視窗**：以一般使用者身分執行，經 `/run/wgal/watchdog.sock` 和 WatchDog 通訊，本身不持有任何權限。視窗只連 root 擁有的 socket。
- **命令列 `wgal`**：WGAL 的命令列，用英文或 `wgal language <tag>` 選的語言（4.0.0 起），`/opt/wgal/wgal`。它和視窗一樣只連 root 擁有的 socket、只送出請求，本身不持有任何權限。
  - 以存取清單上的帳號執行，**不要用 sudo**：root 是管理員，不受存取清單的隧道限制。
  - 沒有桌面的電腦（`--no-window`）就只用它。`wgal --help` 列出所有命令。
  - `wgal` 視為使用者本人（擁有者 2026-11-18 的決定）：任何以那個帳號執行它的程式，包括有 shell 的 Agent，都能做使用者能做的事，包括讀 Chat ID、改 Telegram 設定。MCP 的開關只限制 `wgal-mcp`。
- **WatchDog 接受的對象**：root，以及存取清單中的帳號。其他帳號送來的請求，一律回覆「不在本機安裝的存取清單中」，介面會顯示這句話。
- **秘密**：Bot Token 存在 `/var/lib/wgal/notify/telegram-bot-token`，權限 0600，資料夾 0700。這是權限保護，不是加密。Token 由視窗或 `wgal notify token set` 設定，`wgal` 不從命令列或環境變數讀 Token。

## 檔案

| 路徑 | 內容 | 由誰寫 |
|---|---|---|
| `/opt/wgal/` | `wgal-watchdog`、`wgal-desktop`、`wgal-mcp`、`wgal`、`wgal.png`、`uninstall.sh`、`README.md`、`README.zh-TW.md`；安裝與升級期間另有 `.<名稱>.new`、`.<名稱>.old` 與 `.upgrade.record`，完成後刪除 | 安裝腳本 |
| `/usr/local/bin/wgal` | 連到 `/opt/wgal/wgal` 的連結，所以可以直接打 `wgal`。只在那裡沒有東西、或已經是這個連結時建立；那裡是別的東西時不動，安裝的最後幾行改給完整路徑 `/opt/wgal/wgal` | 安裝腳本；`uninstall.sh` 只在它仍指向 `/opt/wgal/wgal` 時刪除 |
| `/etc/systemd/system/wgal-watchdog.service` | 服務單元 | 安裝腳本 |
| `/etc/wgal/access.conf` | 存取清單：`user <UID>` 與 `tunnel <介面>`，一行一筆 | 安裝腳本 |
| `/var/lib/wgal/`（0700） | `state`、`notify`、`events`，升級前的備份 `backup/state-<時間>` | WatchDog（`StateDirectory`） |
| `/run/wgal/`（0755） | `watchdog.sock`（0666） | WatchDog（`RuntimeDirectory`） |
| `/usr/share/applications/wgal.desktop`、`/etc/xdg/autostart/wgal.desktop` | 應用程式選單與登入時自動開啟。`--no-window` 不安裝它們，並刪除 WGAL 先前安裝的這兩個 | 安裝腳本 |

## 存取清單

- 只有 root 能修改 `access.conf`。WatchDog 只在啟動時讀它，改了要 `sudo systemctl restart wgal-watchdog`。
- 檔案或 `/etc/wgal` 不是 root 專屬、是連結或格式不對時，整份不採用：只有 root 能使用，原因寫在 journal（只寫行號與問題，不引用內容）。
- 要允許新的隧道，就重跑 `sudo ./install.sh --tunnel <全部要允許的介面>`（沒有桌面的電腦另外加 `--no-window`）。這個參數會重寫整份清單。

## 用勾選清單安裝（擁有者 2026-11-12 的需求）

- **給一般使用者**：在解壓縮後的資料夾，從自己的帳號執行 `./setup.sh`。
  - sudo 只問一次密碼；接著列出這台電腦的 wg-quick tunnel（`install.sh --list-tunnels` 的結果），用 whiptail 的勾選清單（空白鍵勾選、Enter 確認）選要交給 WGAL 的 tunnel，再以這些 tunnel 與執行它的帳號（`SUDO_USER`）執行 `install.sh`。沒有 whiptail 時改用編號清單。
  - 已經安裝過時，先問要不要維持目前的清單：維持就照原樣升級（不給 `--tunnel`），重新選擇會取代整份清單。WGAL 不接受的名稱會列出但不能選。
  - 沒有圖形桌面時（見下面「沒有桌面的電腦」）先問要不要不裝視窗。
  - `./setup.sh --dry-run` 不需要 sudo，只印出會執行的指令。系統語言是中文時說繁體中文，其他語言說英文。
- **給 Agent**：`./install.sh --list-tunnels` 印出 JSON，不改任何東西：`{"complete":true,"tunnels":[{"name":"wg0","status":"running","starts_at_boot":true,"accepted":true}]}`，一條 tunnel 一行。
  - 以 root 執行時列出每個 `/etc/wireguard/<名稱>.conf`（`complete` 為 true）；一般使用者讀不到那個資料夾，只列出 systemd 知道的 wg-quick 單元與正在執行的 WireGuard 介面（`complete` 為 false）。只讀名稱，從不讀設定檔的內容。
  - Agent 把清單給使用者選，再用一次 sudo 執行 `sudo ./install.sh --tunnel <選的介面>`（沒有桌面的電腦加 `--no-window`）。`accepted` 只看名稱；`--tunnel` 還需要對應的 `.conf`。
- 已在 WSL 的 Ubuntu 24.04 測過：一般使用者的 `--list-tunnels`；以暫存資料夾代替 `/etc/wireguard` 的完整列出（連結與不合格的名稱被排除）；`setup.sh --dry-run` 的編號清單（中、英文）。
- 視窗需要的 Ubuntu 套件：`libwebkit2gtk-4.1-0`（視窗，沒有它就打不開）與 `libayatana-appindicator3-1`（右上角的圖示）。
  - `install.sh` 在改任何東西之前檢查（`--no-window` 時不檢查）。
  - 缺少時，在終端機詢問、或依 `--install-runtime`（`setup.sh` 以 whiptail 詢問後加上）用 `apt-get` 只安裝缺少的那幾個；沒有同意就以 65 停下，並印出 `sudo apt install …`，也提示沒有桌面的電腦可以用 `--no-window`（擁有者 2026-11-12 的決定：在 Ubuntu 26.04 測試機上，視窗因缺少 WebKitGTK 而打不開）。
  - 移除 WGAL 時不移除它們。
- 2026-11-12 在一台 Ubuntu 26.04 實機上，以一般使用者經 SSH 測過（PASS，不需要 sudo）：`--list-tunnels`（`complete` 為 false，列出執行中的 tunnel）；多給參數被拒絕（64）；`setup.sh --dry-run` 的 whiptail 勾選清單：勾選後按「安裝」印出會執行的指令（0），什麼都沒勾就按「安裝」被拒絕（10），「取消」（10），中、英文。以 root 列出與以 `setup.sh` 實際安裝，見上面的狀態。

## 沒有桌面的電腦（`--no-window`，擁有者 2026-11-18 的決定）

伺服器之類沒有桌面的 Ubuntu 也支援，WGAL 這時只用命令列 `wgal`：

```bash
sudo ./install.sh --no-window --tunnel <介面>[,<介面>...] [--user <帳號>]
```

- 不檢查、也不安裝視窗需要的套件；不安裝應用程式選單與登入自動開啟，並刪除 WGAL 先前安裝的這兩個（它們會開啟這台電腦顯示不了的視窗）。
- `wgal-desktop` 照樣複製到 `/opt/wgal`（程式集合不變），但沒有東西會開啟它。
- 不能和 `--install-runtime` 一起用（64，什麼都不改）。
- **不會記住**：每次執行只依它自己的選項。沒有桌面的電腦每次升級都要再給 `--no-window`；沒給時就走有視窗的路徑：檢查視窗需要的套件（缺少時在終端機詢問，不在終端機就以 65 停下），也會裝回選單與登入自動開啟。
- `setup.sh` 在沒有圖形桌面時詢問：`graphical.target` 不是 active，而且沒有 `DISPLAY` 與 `WAYLAND_DISPLAY`。預設是不裝視窗；同意就傳 `--no-window`，不再問視窗的套件。
- 成功時最後幾行說：`Done, without the window. As <帳號> (not with sudo): "wgal status" shows WGAL, "wgal --help" the rest.`（`/usr/local/bin/wgal` 建立不了時，`wgal` 換成完整路徑）。
- 之後以存取清單上的帳號（**不要用 sudo**）執行 `wgal status` 確認，再用 `wgal discover`、`wgal register`、`wgal set <隧道> --recovery-target <位址>`、`wgal auto-recovery` 與 `wgal notify` 設定。步驟見 `INSTALL.zh-TW.md`。

## 第一次安裝

還沒有 `/etc/systemd/system/wgal-watchdog.service` 時是第一次安裝。準備的步驟和下面的升級相同（複本與 `.upgrade.record`）。然後換上存取清單、程式與 unit，`daemon-reload`、`enable`，把 `started yes` 寫進 `.upgrade.record` 並讀回確認之後，才啟動服務並檢查 socket。

在那之前任何一步失敗（包括 Ctrl-C），腳本**不啟動**服務：先關掉 WGAL 自己的開機啟動（`systemctl disable wgal-watchdog.service`，再確認），保留複本與記錄，以 1 結束，`!!!` 訊息說是在哪一步失敗。修正原因後再執行一次。

## 升級與中途失敗

已經有 `/etc/systemd/system/wgal-watchdog.service` 時，`install.sh` 是升級。

停止服務之前先做完能做的：
- 四個程式（`wgal-watchdog`、`wgal-desktop`、`wgal-mcp`、`wgal`）與 unit 先複製到 `/opt/wgal/.<名稱>.new`，在位的另外複製成 `/opt/wgal/.<名稱>.old`。都以 SHA-256 核對，雜湊記在 `/opt/wgal/.upgrade.record`，最後一行 `started no`。
- 從 3.0.0 之前的版本升級時，在位的那一組沒有 `wgal`：記錄裡它的舊版雜湊是 `-`。之前的執行留下的、沒有 `wgal` 的記錄，也照那一組來讀。
- 選單與自動啟動（`--no-window` 時改為刪除）、圖示、`uninstall.sh` 與兩份 README 先換好。
- 給了 `--tunnel`／`--user` 時，新的存取清單先完整寫成 `/etc/wgal` 裡的新檔。

然後停止服務、備份狀態、以一次改名換上存取清單（不先刪舊的）、以改名換上四個程式、寫入 unit、`daemon-reload`、`enable`，最後啟動並檢查 socket；成功後刪除 `.new`、`.old` 與 `.upgrade.record`。新的一組換上並開始啟動之後，才建立 `/usr/local/bin/wgal` 的連結（見「檔案」，第一次安裝也一樣）；由下面的 `trap` 結束的執行不建立連結。

之前的執行留下的 `.new`／`.old`，先依它的 `.upgrade.record` 確認在位的是那次記下的完整舊版或完整新版（或那次之前根本沒有安裝），才刪除並重新複製；沒有 `.upgrade.record` 的複本是在準備途中就停下的那次留下的（那時在位的檔案還沒動過），直接刪除。無法確認時，腳本在停止服務之前、改 WGAL 的任何檔案之前就以 65 停下，並說明怎麼做：那次還沒啟動新版時，說明怎麼把舊版放回去（那些複本可能是唯一完整的一組）；那次可能已經啟動新版時，只說明怎麼放回完整的新版，或原樣保留交給擁有者。

`.upgrade.record` 的 `started` 一行記錄新版是否可能已經啟動過：準備時寫 `no`，開始啟動新版之前改成 `yes`；記錄只在完整的一組已啟動後才和複本一起刪除。之後的執行讀到的記錄不是 `started no`（或沒有這一行）時，就當成新版可能已經執行過、改過狀態：這次執行同樣**絕不啟動舊版**，也把 `started yes` 繼續記下去。

每次要啟動新版之前（第一次安裝的啟動、升級的第一次啟動，或 trap 復原時再啟動一次），腳本都先把 `started yes` 寫進 `.upgrade.record`，再讀回來確認；確認不了（例如磁碟已滿或檔案系統變成唯讀）就**不啟動**服務，保留 `/opt/wgal` 裡的複本與 `.upgrade.record`，以 1 結束，並照實說明這次有沒有啟動過新版：把完整的新版留在原位、不要把 `.<名稱>.old` 放回去，修正原因後在 release 資料夾再執行一次腳本；或原樣保留，交給擁有者判斷。

**從停止服務到啟動後的檢查做完之前，任何一步失敗（包括 Ctrl-C）**，`trap` 接手：

1. 以 SHA-256 判斷在位的是**完整的舊版**還是**完整的新版**（新版這時還沒執行過）。只換了一部分時，從 `.old` 把舊的放回去（舊版沒有的檔案刪除，例如從 3.0.0 之前的版本升級時的 `wgal`）。
2. `daemon-reload` 後啟動那一組並檢查 socket，並說明在哪一步失敗、現在跑的是舊版還是新版，以及存取清單：沒換上時「舊的清單仍然有效，這次給的沒有套用」；已換上時保留新的，**不會把舊清單放回去**（不會恢復剛撤銷的授權）。
3. 不碰 WGAL 保存的狀態，不拿備份蓋回去。
4. 照常比對 wg-quick 單元。服務在執行時以 4 結束（1、2 優先）；沒有執行時以 1 結束並說明現在沒有任何監看。

新版的啟動一開始，新版就可能已經執行過、改過狀態：這之後的失敗（例如啟動或等 socket 時按 Ctrl-C）**不會把舊版放回去**，`trap` 再啟動一次在位的完整新版，並說明新版可能已經執行過。

兩組都湊不齊時不啟動，保留 `.old`、`.new` 與 `.upgrade.record`，以 1 結束並說明。新版還沒啟動過時，之後再執行會以 65 拒絕（不刪這些複本）：先修正原因，自己把舊版放回去（把 `/opt/wgal` 裡還在的每個 `.<名稱>.old` 移回原位，unit 的移到 `/etc/systemd/system/wgal-watchdog.service`；`.upgrade.record` 裡舊版雜湊為 `-` 的程式刪除），再執行一次。新版已經啟動、之後才沒有出現 socket 時不會自動退回舊版：新版可能已經改過狀態。

新版**可能已經執行過**（這次或之前某次已開始啟動新版）時，就算在位的是完整的舊版，`trap` 也不啟動它、不把舊版放回去，保留複本與 `.upgrade.record`，以 1 結束，並說明：**不要把舊版放回去、不要啟動它**；先修正原因，再從發布包執行一次，讓它換上完整的新版；在位的不是完整的一組時，再執行會以 65 拒絕，要先自己從同一個發布包把 `wgal-watchdog`、`wgal-desktop`、`wgal-mcp`、`wgal` 放進 `/opt/wgal`、`wgal-watchdog.service` 放到 `/etc/systemd/system/wgal-watchdog.service`（各自要符合記錄裡的新版雜湊）；或原樣保留，交給擁有者判斷。

上面三種**不啟動**的情況（記錄寫不進去、兩組都湊不齊、新版可能已經執行過而在位的不是完整新版），腳本在說明之前都先執行 `systemctl disable wgal-watchdog.service`，再以 `systemctl is-enabled` 確認，免得 systemd 在下次開機時自己啟動腳本剛拒絕啟動的那一組。它只動 WGAL 自己的 unit，從不動任何 `wg-quick@` 單元與它們的開機啟動。
- `!!!` 訊息會說已關閉開機啟動，下一次通過檢查的執行會再 `enable`（換上新的一組時本來就會 `enable`；`trap` 復原時啟動哪一組，也先 `enable` 再啟動）。
- 停止之前就以 65 拒絕的執行維持 disabled。
- 關不掉時訊息直說關不掉、systemd 可能在下次開機時啟動在位的那一組：請在重新開機之前修正原因並再執行一次腳本，或原樣交給擁有者。

`install.sh` 的結束碼：
- 0：成功；
- 1：服務沒有以 socket 運作；
- 2：wg-quick 單元有變化或結束時讀不到（和 1 同時發生時回 2）；
- 4：升級在停止之後有一步失敗，服務已用在位的完整一組啟動回來（只在 1、2 都不成立時）；
- 64：用法錯誤；
- 65：停止服務之前的檢查或準備沒過：服務與使用中的 WGAL 檔案維持原樣，但 `/opt/wgal` 裡的複本、選單與自動啟動（`--no-window` 時是刪除它們）、`uninstall.sh`、`/etc/wgal` 裡準備好的存取清單，或視窗需要的套件，可能已經換成新的。

這段流程的測試是 `tests/install-recovery.test.sh`：以一般使用者執行（不要用 sudo），把 `install.sh` 當函式庫載入（這時它什麼都不執行），路徑改到自己的暫存資料夾，以替身取代 `systemctl`、`pgrep` 與檔案操作，逐步注入失敗。替身只在測試裡，`install.sh` 沒有選用它們的選項或變數。它不是實機的升級測試。

## 退回 4.0.0 以前的版本

用過 `wgal language <tag>` 之後（擁有者 2026-11-19 的決定），`/var/lib/wgal/notify/notify.json` 存著 `wgal` 的語言，4.0.0 以前的 WatchDog 讀不懂這個檔：通知會停在關閉、也不接受通知設定的變更，直到檔案修好。

- 安裝舊版本之前，先在 4.0.0 以上執行 `wgal language --clear`（Telegram 訊息維持原來的語言），並確認 `wgal language` 顯示英文是預設。
- 舊版本已經因此在通知關閉的狀態下執行時：再安裝 4.0.0 以上，執行 `wgal language --clear`，然後再安裝舊版本。

升級前的備份（`/var/lib/wgal/backup/state-<time>`）不能用來繞過：用過 `wgal language` 之後的升級所做的備份，裡面也有這個語言。這份 README 不提供在 Ubuntu 放回備份的步驟。

## 紀錄

- WatchDog 自己的紀錄：`journalctl -u wgal-watchdog`。
- 事件紀錄：`/var/lib/wgal/events/events.jsonl`，只有 root 能讀；視窗的「紀錄」頁與 `wgal log` 會顯示它。

## 結束碼（`systemctl status` 的 `status=`）

| 碼 | 意思 |
|---|---|
| 1 | `/var/lib/wgal` 不是 root 的 0700 資料夾，或 `/run/wgal` 不是 root 專屬 |
| 2 | `state`、`notify` 或 `events` 不是 root 的 0700 資料夾 |
| 3 | 狀態檔無法開啟 |
| 4 | 監控迴圈無法啟動 |
| 5 | 不是以 root 執行（不會自動重啟） |
| 11 | 監控迴圈停止回應；systemd 會重啟它 |

## 和 Windows 版的差異（尚未實作）

- 從睡眠喚醒、網路切換時，不會重新開始寬限期；仍由探測失敗本身判斷是否故障。
- 沒有安裝檔（`.deb`），以腳本安裝（擁有者 2026-11-05 決定）。

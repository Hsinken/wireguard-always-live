# WG Always Live 安裝與移除指南（給人，也給代為安裝的 Agent）

[English](INSTALL.md) | **繁體中文**

這份文件放在每個發布壓縮包裡，英文版是 `INSTALL.md`。照著做就能在 Windows 11、macOS 或 Ubuntu（有桌面或沒有桌面的伺服器）上安裝、升級、驗證與移除 WG Always Live（WGAL）。每個平台的細節在同一個資料夾的 `README.zh-TW.md`。

WGAL 不是 VPN，也不取代官方 WireGuard。它只監看官方 WireGuard 已有的隧道是否真的通，在使用者允許時重新連線出問題的那一條。

## 給 Agent 的規則（先讀）

1. **只照這份文件的指令做**：
   - 不改官方 WireGuard 的任何設定、金鑰、隧道或開機自動連線；
   - 不手動連線或斷線 VPN；
   - 不安裝其他套件，也不改防火牆、路由或 DNS。
2. **不要透過要監看的那條 VPN 遠端操作**：安裝時服務會短暫停止與啟動，但腳本本身不會碰任何隧道。請在本機，或經由不依賴那條 VPN 的連線操作。
3. **先問使用者**：
   - 用哪個帳號使用 WGAL（開視窗，或用命令列 `wgal`）；
   - 要允許這個帳號登記、並交給自動復原的隧道（Windows 與 Ubuntu 需要；macOS 不需要）；
   - Ubuntu：這台電腦有沒有桌面。沒有桌面時以 `--no-window` 安裝（見「沒有桌面的 Ubuntu」）。

   不要自己猜。隧道不要叫使用者打名稱：先用不需要管理員權限、也不改任何東西的指令列出這台電腦的隧道（Windows `.\install.cmd -ListTunnels`，Ubuntu `./install.sh --list-tunnels`），把清單給使用者勾選，再用**一次**管理員權限執行安裝。
4. **不要讀或印出秘密**：Telegram Bot Token、WireGuard 設定檔內容、私鑰都不要讀。腳本也不讀它們。使用者要 Telegram 通知時，Bot Token 由使用者自己輸入：在視窗裡，或在自己的終端機執行 `wgal notify token set`。不要把 Token 寫在命令列上。
5. **每一步都看結束碼**：0 才算成功。不是 0 就停下來，把腳本印出的訊息原文交給使用者，不要自行「修好」再重試。
   - 訊息若說新版可能已經執行過（may already have run），不要把舊版放回去或啟動它；只照訊息放回完整的新版，或原樣保留交給擁有者。
   - 訊息若說記錄寫不進去、因此沒有啟動（could not be marked、could not be written … NOT started），把完整的新版留在原位、不要放回舊版；照訊息排除原因（磁碟空間、唯讀、別的程式開著檔案）後再執行一次，或原樣保留交給擁有者。
   - 訊息若說腳本沒有啟動 WatchDog（NOT started），它也已關閉 WGAL 自己的自動啟動（Windows 服務設為 Disabled、Ubuntu `systemctl disable wgal-watchdog.service`、macOS `launchctl disable` WatchDog 的 label），免得下次開機或登入時作業系統自己啟動那一組；照訊息處理後再執行一次腳本，通過檢查時就會重新開啟。
   - 訊息若說自動啟動關不掉（could NOT be turned off），在重新開機、登出之前先照訊息處理並再執行一次，或原樣交給擁有者；不要自己改 WGAL 服務的啟動設定，也不要碰任何 WireGuard 隧道的開機啟動。
6. **需要管理員權限時，請使用者自己確認，而且只請求一次**：Windows 的 UAC，以及 Ubuntu／macOS 的 sudo 密碼，都由使用者輸入。列出隧道不需要管理員權限，所以整個安裝只需要使用者同意一次。
7. **使用者想自己點選安裝時**：Windows 請他在解壓後的資料夾雙擊 `setup.cmd`，Ubuntu 請他執行 `./setup.sh`。兩者都只問一次權限，列出隧道讓他勾選，再執行同一支安裝腳本。
8. **命令列 `wgal` 等於使用者本人**：以存取清單上的帳號執行的任何程式，包括有 shell 的 Agent，都能用 `wgal` 做使用者能做的事，包括讀 Telegram 的 Chat ID、改 Telegram 設定。只做使用者要求的事；使用者沒有要求，不要送測試訊息。`wgal` 用使用者為它選的語言輸出；需要英文的輸出時，在任何指令加上 `--lang en`；使用者沒有要求，不要改使用者的語言（`wgal language`）。

## 去哪裡取得

- **發布包**：從 [Releases 頁面](https://github.com/Hsinken/wireguard-always-live/releases) 下載符合平台的壓縮包，用頁面上公布的 SHA-256 核對。解開之後，再用 `RELEASE.txt` 核對包內的檔案（見下一節）。前一個核對才證明下載的檔案是真的；`RELEASE.txt` 只證明檔案與建置紀錄一致。
- **從原始碼**：在要安裝的那台電腦上，照本指南最後的「從原始碼建置發布包」建置，再用產生的發布包照常安裝。

## 壓縮包內容

| 平台 | 檔名 | 內容 |
|---|---|---|
| Windows 11（x64） | `wgal-<版本>-windows-x64.zip` | `wgal-watchdog.exe`、`wgal-desktop.exe`、`wgal-mcp.exe`、`wgal.exe`；`install`、`uninstall`、`revert`、`setup` 各有 `.ps1` 與 `.cmd`；說明文件；`RELEASE.txt` |
| macOS | `wgal-<版本>-macos-<arm64 或 x86_64>.zip` | `WG Always Live.app`（`wgal-watchdog`、`wgal-mcp`、`wgal` 在 app 裡）；`install.sh`、`uninstall.sh`；說明文件；`RELEASE.txt` |
| Ubuntu | `wgal-<版本>-linux-x86_64.tar.gz` | `wgal-watchdog`、`wgal-desktop`、`wgal-mcp`、`wgal`、`wgal-watchdog.service`、`wgal.desktop`、`wgal.png`；`install.sh`、`uninstall.sh`、`setup.sh`；說明文件；`RELEASE.txt` |

程式：
- `wgal-watchdog`：背景服務，探測與復原隧道，發 Telegram 訊息；
- `wgal-desktop`：視窗（使用者自己的帳號，沒有任何權限）；
- `wgal-mcp`：給 AI Agent 用的 MCP 伺服器，使用者打開之前一律被拒絕；
- `wgal`：命令列，能做視窗對 WatchDog 做的每一件事（3.0.0 起）；預設英文，4.0.0 起可以選 13 種語言。

`RELEASE.txt` 寫著：
- 版本：4.2.1 起整套（介面、WatchDog、`wgal-mcp` 與 `wgal`）共用一個，也就是檔名裡的那一個；
- 建置的 commit；
- 每個檔案的 SHA-256（macOS 是腳本、說明文件與 app 的 `Contents/MacOS` 裡的程式）。

解壓後可以先核對雜湊：Windows 用 `Get-FileHash`、macOS 用 `shasum -a 256`、Ubuntu 用 `sha256sum`。

這些檔案沒有程式碼簽章（擁有者 2026-11-05 決定）：
- Windows 可能出現 SmartScreen 警告；
- macOS 由安裝腳本移除這個 app 自己的下載隔離標記，見 macOS 一節。

## 先決條件

- **官方 WireGuard 已安裝，並已加入要監看的隧道**：
  - Windows：WireGuard for Windows。只有「已啟用」的隧道才有服務可登記。
  - macOS：Mac App Store 的 WireGuard。
  - Ubuntu：`wireguard-tools`，設定檔在 `/etc/wireguard/<名稱>.conf`。WGAL 的視窗另外需要 Ubuntu 套件 `libwebkit2gtk-4.1-0`（沒有它視窗打不開）與 `libayatana-appindicator3-1`（右上角的圖示）；缺少時安裝腳本會詢問是否用 `apt-get` 安裝，見 Ubuntu 一節。沒有桌面時都不需要，見「沒有桌面的 Ubuntu」。
- **先退出 WGAL 視窗**（升級或移除時）：系統匣或選單列圖示的選單 →「退出介面（背景監控繼續）」。也讓執行中的 `wgal` 命令結束，並關掉使用 `wgal-mcp` 的 Agent。

---

## Windows 11

### 安裝或升級

**使用者自己操作（建議）**：把 zip 解壓到本機資料夾，雙擊 `setup.cmd`。Windows 詢問一次系統管理員權限後，出現列出**啟用中**隧道的小視窗，勾選後按「安裝」（已安裝時是「升級」，預設維持目前的清單）。最後的黑色視窗顯示安裝結果，按 Enter 關閉。`setup.cmd -DryRun` 只顯示視窗與會執行的指令，不改任何東西。

**Agent 代為安裝**：

1. 把 zip 解壓到本機資料夾，例如 `C:\Users\<你>\wgal-<版本>-windows-x64`。
2. 在那個資料夾，用一般（不需要提升權限）的 PowerShell 列出隧道：

```powershell
.\install.cmd -ListTunnels
```

   它印出一行 JSON，不改任何東西：`{"complete":false,"tunnels":[{"name":"office","status":"running","starts_at_boot":true,"accepted":true}]}`。只列出啟用中的隧道（沒有啟用的隧道沒有服務，`complete` 因此為 false）；`accepted` 為 false 的名稱 WGAL 不接受，不能選。把清單給使用者勾選。
3. 以**系統管理員身分**執行下面其中一種（例如以 `Start-Process -Verb RunAs` 啟動，使用者同意一次 UAC）：

```bat
.\install.cmd -Tunnel <隧道名稱>[,<隧道名稱>...]
```

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\install.ps1 -Tunnel <隧道名稱>[,<隧道名稱>...]
```

4. 在 PowerShell 裡一定要寫成 `.\install.cmd`，前面的 `.\` 不能省：PowerShell 不會執行目前資料夾裡的檔案，只寫 `install.cmd` 會去 PATH 上找，可能執行到別的工具同名的腳本（例如 nvm-windows 的 `install.cmd`，它會要求輸入路徑並改動系統的 PATH）。遇到不是 WGAL 的提問，按 Ctrl+C 結束，什麼都不要輸入。`.\` 在命令提示字元也能用。切換資料夾時，PowerShell 用 `cd <資料夾>`，命令提示字元用 `cd /d <資料夾>`。

參數：
- 多條隧道用逗號分隔，例如 `-Tunnel office,home`。
- 用另一個管理員帳號提升權限時，加上 `-User <使用 WGAL 的帳號 SID>`。那個帳號自己的視窗執行 `whoami /user` 可以看到 SID。
- 升級時不給 `-Tunnel`，存取清單維持原樣。

結束碼：
- 0：成功；
- 1：服務沒有以 pipe 運作；或停止服務之前的檢查沒過，腳本以錯誤訊息停下。若訊息說上一次升級留下的複本（`*.exe.old`、`*.exe.new`、`upgrade.record`）無法確認，腳本沒有停止服務、沒有改任何檔案；照第 5 條把訊息原文交給使用者；
- 2：WireGuard 服務在執行期間有變化；
- 3：pipe 名稱被別的程序佔用；
- 4：升級在停止服務之後有一步失敗；腳本已用當時在位的完整一組執行檔把服務啟動回來，並以 `!!!` 開頭的行說明是舊版還是新版、用的是哪一份存取清單（只在 1、2、3 都不成立時）。照第 5 條把訊息原文交給使用者，原因排除後再執行一次。

### 驗證

- `sc.exe query WGALWatchdog` 顯示 `RUNNING`。
- 從**一般（未提權）**的工作階段開啟 `C:\Program Files\WGAL\wgal-desktop.exe`；畫面上方顯示「背景監控正常」。
- 或以那個帳號在一般的 PowerShell 執行 `& 'C:\Program Files\WGAL\wgal.exe' --lang en status`，第一行以 `WGAL watchdog <版本>` 開頭（`--lang en`：不論為 `wgal` 選了什麼語言都用英文）。腳本不改 PATH，所以 `wgal.exe` 要寫完整路徑。

### 移除

以系統管理員身分執行：

```bat
.\uninstall.cmd
```

- 要連資料一起刪除時加 `-RemoveData`。
- 服務卡住停不下來時，先請使用者同意，再執行 `.\revert.cmd -Tunnel <登記的隧道>[,...]`：它結束服務，並把被停下的那幾條隧道啟動回來。之後再重跑 `.\uninstall.cmd`。

---

## Ubuntu

這一節是有桌面的電腦。沒有桌面時，照下面的「沒有桌面的 Ubuntu」。

### 安裝或升級

**使用者自己操作（建議）**：解壓後在那個資料夾執行 `./setup.sh`（不要加 sudo，它會自己用 sudo 問一次密碼）。它列出隧道，用勾選清單（空白鍵勾選、Enter 確認）選好後安裝；已安裝時先問要不要維持目前的清單。沒有圖形桌面的電腦上，它會問要不要不裝視窗。`./setup.sh --dry-run` 只顯示選項與會執行的指令，不改任何東西。

**Agent 代為安裝**：

1. 解壓：`tar -xzf wgal-<版本>-linux-x86_64.tar.gz && cd wgal-<版本>-linux-x86_64`
2. 列出隧道：`./install.sh --list-tunnels`。它印出 JSON，一條隧道一行，不改任何東西。一般使用者讀不到 `/etc/wireguard`，所以只列出 systemd 知道的與正在執行的隧道（`complete` 為 false）；需要完整清單時，用接下來安裝時同一次的 sudo 執行 `sudo ./install.sh --list-tunnels`。把清單給使用者勾選。
3. 執行：

```bash
sudo ./install.sh --tunnel <介面名稱>[,<介面名稱>...]
```

參數：
- 使用 WGAL 的帳號預設是執行 sudo 的帳號；要指定其他帳號時加 `--user <帳號>`。不能是 root。
- 已經有存取清單時（升級，或移除時保留了資料後重新安裝），不給 `--tunnel`／`--user` 就維持原樣。
- 視窗需要的套件（`libwebkit2gtk-4.1-0`、`libayatana-appindicator3-1`）沒有安裝時：在終端機執行會先詢問是否用 `apt-get` 安裝；Agent 代為安裝時，先問使用者同意，同意才加 `--install-runtime`。不同意或沒有加，腳本在改任何東西之前停下，並印出要執行的 `sudo apt install …`。腳本只裝這兩個套件中缺少的，移除 WGAL 時不會移除它們。

結束碼：
- 0：成功；
- 1：服務沒有以 socket 運作；
- 2：wg-quick 單元在執行期間有變化，或結束時讀不到它們的狀態（和 1 同時發生時回 2）；
- 4：升級在停止服務之後有一步失敗；腳本已用當時在位的完整一組（舊版或新版，`!!!` 開頭的行會說明）把服務啟動回來（只在 1、2 都不成立時）；
- 64：用法錯誤；
- 65：停止服務之前的檢查或準備沒過（包括 systemctl 沒有回應，或上一次未完成的升級留下的複本無法確認在位的是完整的一組）。服務與使用中的 WGAL 檔案維持原樣，但 `/opt/wgal` 裡準備好的複本（`.<名稱>.new`、`.<名稱>.old`、`.upgrade.record`）、選單與登入自動啟動、`/opt/wgal/uninstall.sh`、`/etc/wgal` 裡準備好的存取清單，或使用者同意安裝的視窗套件，可能已經建立或換新。照第 5 條把訊息原文交給使用者。

安裝的東西：

| 位置 | 內容 |
|---|---|
| `/opt/wgal/` | 四個程式、圖示、`uninstall.sh`、說明文件 |
| `/usr/local/bin/wgal` | 指向 `/opt/wgal/wgal` 的連結；只在那裡沒有東西、或已經是 WGAL 的連結時建立，否則結尾的訊息會給完整路徑 |
| `/etc/systemd/system/wgal-watchdog.service` | WatchDog 服務，以 root 執行，開機自動啟動 |
| `/etc/wgal/access.conf` | 存取清單 |
| `/var/lib/wgal/` | 資料 |
| `/run/wgal/watchdog.sock` | 本機 socket |
| 應用程式選單與登入自動啟動 | 視窗 |

### 驗證

- `systemctl is-active wgal-watchdog` 顯示 `active`。
- `journalctl -u wgal-watchdog -n 20` 有 `running` 這一行。
- 以使用者自己的身分（**不要用 sudo**）從應用程式選單開啟 WG Always Live；畫面上方顯示「背景監控正常」。或執行 `wgal --lang en status`，第一行以 `WGAL watchdog <版本>` 開頭。

### 移除

```bash
sudo /opt/wgal/uninstall.sh
```

- 要連 `/var/lib/wgal` 與 `/etc/wgal` 一起刪除時加 `--remove-data`。
- `/usr/local/bin/wgal` 只在是 WGAL 的連結時移除。
- 只想停用、不移除：`sudo systemctl disable --now wgal-watchdog`。

---

## 沒有桌面的 Ubuntu（伺服器）

沒有桌面時，WGAL 不裝視窗，改用命令列 `wgal`（3.0.0 起支援）。WatchDog 完全相同：自己探測、復原、發 Telegram 訊息。

### 安裝或升級

**使用者自己操作**：在解壓後的資料夾執行 `./setup.sh`（不要加 sudo）。沒有圖形桌面的電腦上，它會問要不要不裝視窗；預設是「要」，之後它加上 `--no-window`。

**Agent 代為安裝**：

1. 照 Ubuntu 一節的第 1、2 步解壓並列出隧道。
2. 執行：

```bash
sudo ./install.sh --no-window --tunnel <介面名稱>[,<介面名稱>...]
```

`--no-window`：
- 不檢查、也不安裝視窗需要的套件；
- 不裝選單與登入自動啟動的項目，並移除之前安裝留下的 WGAL 自己的項目；
- 視窗的程式照樣複製到 `/opt/wgal`，程式的集合不變，只是沒有東西會開啟它；
- **不會記住**：每次執行都依那一次的參數。每次升級都要再給 `--no-window`：`sudo ./install.sh --no-window`。沒有給時就走有桌面的路徑：檢查視窗需要的套件，缺少時在終端機詢問，否則以 65 停下（什麼都沒改）；
- 不能和 `--install-runtime` 一起用（結束碼 64）。

使用 `wgal` 的帳號是執行 sudo 的那個帳號，或 `--user <帳號>` 指定的帳號，不能是 root。請用那個帳號登入伺服器，或加上 `--user`。`--tunnel`、`--user`、結束碼與安裝的東西都和 Ubuntu 一節相同，只是沒有選單與登入自動啟動的項目。

### 驗證

- `systemctl is-active wgal-watchdog` 顯示 `active`。
- `journalctl -u wgal-watchdog -n 20` 有 `running` 這一行。
- 以存取清單上的帳號（**不要用 sudo**）執行 `wgal --lang en status`，第一行以 `WGAL watchdog <版本>` 開頭。`/usr/local/bin/wgal` 建不了時，安裝結尾的訊息會說明；改用 `/opt/wgal/wgal`。

接著照下面的「用命令列設定 WGAL」設定。

### 移除

和 Ubuntu 一節相同：`sudo /opt/wgal/uninstall.sh`，要連資料一起刪除時加 `--remove-data`。

---

## macOS

### 安裝或升級

1. 解壓：在 Finder 雙擊 zip，或執行 `ditto -x -k wgal-<版本>-macos-arm64.zip .`
2. 在 Mac 上以自己的帳號登入，在「終端機」進入資料夾，以**自己的身分**執行（**不要用 sudo**）：

```bash
./install.sh
```

- 腳本把 app 複製到 `/Applications`。沒有寫入權限時，會用 sudo 要求管理員密碼。
- 這個 app 沒有 Apple 開發者簽章。腳本只移除這個 app 的下載隔離標記，不改系統的 Gatekeeper 設定。

結束碼：
- 0：成功；
- 1：WatchDog 沒有以 socket 運作；
- 2：VPN 設定在執行期間有變化；
- 4：升級在停止 WatchDog 之後有一步失敗；腳本已用當時在位的完整 app 把 WatchDog 啟動回來（`!!!` 開頭的行會說明，只在 1、2 都不成立時）；
- 64：用法錯誤；
- 65：停止 WatchDog 之前的檢查或準備沒過（包括上一次未完成的升級留下的舊 app `.WG Always Live.app.old` 無法確認可以刪除）。WatchDog 與在位的 app 維持原樣，但 `~/Library/Application Support/WGAL`（其中的 `uninstall.sh` 與 `upgrade/`）、`~/Library/Logs/WGAL` 與隱藏的 `/Applications/.WG Always Live.app.new` 可能已經建立或換新。照第 5 條把訊息原文交給使用者。

升級時，腳本也把狀態備份到 `~/Library/Application Support/WGAL/backup/state-<時間>`（最多保留 5 份）。

安裝的東西：

| 位置 | 內容 |
|---|---|
| `/Applications/WG Always Live.app` | 介面，`wgal-watchdog`、`wgal-mcp` 與 `wgal` 在 app 的 `Contents/MacOS/` 裡 |
| `~/Library/LaunchAgents/io.github.hsinken.wg-always-live.watchdog.plist` | WatchDog，登入時以使用者身分啟動 |
| `~/Library/LaunchAgents/io.github.hsinken.wg-always-live.window.plist` | 登入時開啟視窗 |
| `~/Library/Application Support/WGAL/` | 資料、socket 與 `uninstall.sh` |
| `~/Library/Logs/WGAL/watchdog.log` | 紀錄 |
| 登入鑰匙圈 | Bot Token |

### 驗證

- `launchctl print gui/$(id -u)/io.github.hsinken.wg-always-live.watchdog | grep state` 顯示 `running`。
- 視窗上方顯示「背景監控正常」。
- 或以自己的身分執行 `"/Applications/WG Always Live.app/Contents/MacOS/wgal" --lang en status`（要有引號），第一行以 `WGAL watchdog <版本>` 開頭。腳本不改 PATH。

### 移除

以自己的身分執行（**不要用 sudo**）。安裝時已把移除腳本複製到 WGAL 的資料夾：

```bash
"$HOME/Library/Application Support/WGAL/uninstall.sh"
```

要連資料、紀錄與鑰匙圈中的 Bot Token 一起刪除時，加 `--remove-data`。

---

## 用命令列設定 WGAL

有桌面時，這些都可以在視窗裡做。沒有桌面，或想用終端機時，`wgal` 做的是同樣的事：它送給 WatchDog 的就是視窗送的那些請求，沒有別的。以使用 WGAL 的帳號執行，不要提升權限、不要用 sudo（提升權限的管理員或 root 不受存取清單的隧道限制）。沒有選擇語言時，`wgal` 用英文（見下面的「語言」）。

| 平台 | 執行方式 |
|---|---|
| Windows | PowerShell 裡 `& 'C:\Program Files\WGAL\wgal.exe' <命令>`，命令提示字元裡 `"C:\Program Files\WGAL\wgal.exe" <命令>` |
| Ubuntu | `wgal <命令>`，或 `/opt/wgal/wgal <命令>` |
| macOS | `"/Applications/WG Always Live.app/Contents/MacOS/wgal" <命令>` |

`wgal help` 列出所有命令，`wgal help <命令>` 說明其中一個。下面的例子都寫成 `wgal`。

### 監看一條隧道

1. `wgal discover`：官方 WireGuard 管理程式有的隧道、WGAL 給每一條的 ID（第一欄），以及哪些已登記。
2. `wgal register <隧道>`：開始監看。Windows 與 Ubuntu 上只有存取清單上的隧道能登記（管理員可以登記任何一條）；macOS 沒有存取清單，你自己的任何一條都可以。
3. `wgal set <隧道> --recovery-target <位址>`：探測必須經這條隧道到達的目標。它是 IPv4 位址（ICMP）或 `位址:連接埠`（TCP 連線），只有經過隧道才到得了，例如對端的一台主機。目前每個系統都只能用位址（ICMP）：WGAL 還不能把 TCP 探測綁定在隧道上，所以 TCP 目標會被探測，但永遠不能證明流量經過隧道。`--observe <位址>`（最多 8 次，會取代整份清單）設定只探測、只顯示的目標，它們不會決定故障或復原。`--interval`、`--timeout`、`--retries`、`--min-fault` 調整探測，範圍見 `wgal help set`。
4. `wgal probe <隧道>`：立刻探測這些目標，看哪些有回應。
5. `wgal auto-recovery <隧道> on`：流量中斷時讓 WGAL 重新啟動這條隧道。和登記一樣，Windows 與 Ubuntu 上只有存取清單上的隧道能交出去。復原目標經這條隧道回應過探測之後才會開啟（十分鐘內；否則維持關閉），`wgal` 會說明。
6. 可選：`wgal start-when-stopped <隧道> on`，隧道被 WGAL 以外的程式停止時也啟動回來；`wgal retry <隧道> --limit <N>|forever --interval <S>` 設定復原失敗後多久再試。

`wgal status` 每行一項顯示 WatchDog 與每條隧道；`wgal show <隧道>` 顯示一條隧道的全部；`wgal log` 顯示事件紀錄。`wgal monitor <隧道> off` 與 `wgal pause` 停止探測（一條或全部），不忘記任何設定；`wgal unregister <隧道> --yes` 讓 WGAL 忘記這條隧道，從不碰隧道本身。

### Telegram 通知

以下由使用者在自己的終端機執行（Agent 的第 4 條）：

1. `wgal notify device "<名稱>"`：訊息裡這台電腦的名稱。沒有設定（或只有空白）時，4.1.0 起訊息使用電腦的主機名稱，`wgal notify` 會顯示它；`wgal notify device ""` 改回用主機名稱。
2. `wgal notify chat <Chat ID>`：要發送到的 Telegram 對話。
3. `wgal notify token set`：不回顯地詢問 Bot Token（標準輸入不是終端機時讀一行）。它從不從命令列或環境變數取得 Token。`wgal` 在詢問時被強制結束、終端機不再顯示輸入時，Linux 與 macOS 用 `stty echo` 恢復。
4. `wgal notify on`，再用 `wgal notify test` 送一則測試訊息。

沒有選擇語言時訊息是英文：`wgal language <tag>` 同時為 `wgal` 與訊息選一種語言（見「語言」），`wgal notify language <tag>` 只改訊息的語言。有視窗的電腦上，WatchDog 還在用內建的英文時，視窗會交出它自己的語言；用 `wgal` 選的語言會一直使用，直到有人在視窗或用 `wgal` 選別的。

訊息裡的時間使用電腦的時區，4.1.0 起 WatchDog 在每則訊息發出前重讀一次：系統的時區改了，Ubuntu 與 macOS 下一則訊息就會用新的，Windows 在五分鐘內。`wgal host --time-zone <名稱>`（IANA 名稱，例如 `Asia/Taipei`）讓 WGAL 不論系統的時區一律使用那個時區，`wgal host --time-zone system` 改回跟隨系統。

### 語言

`wgal` 與 Telegram 訊息有 13 種語言：`zh-TW`、`en`、`zh-CN`、`ja`、`ko`、`de`、`es`、`pt-BR`、`tr`、`fr`、`ru`、`id`、`vi`。還沒有選擇時兩者都是英文。

- `wgal language`：顯示 `wgal` 用的語言、訊息用的語言，以及可以選的 13 種。
- `wgal language <tag>`（例如 `wgal language zh-TW`）：從此 `wgal` 用這種語言，訊息也改用這種語言。WatchDog 把它和通知設定存在一起，所以使用同一個 WatchDog 的帳號都一樣：Windows 與 Ubuntu 是這台電腦上的每個帳號；macOS 每個使用者有自己的 WatchDog，只影響那個使用者。視窗從不改它；在視窗裡改用視窗的語言，只會改訊息的語言。
- `wgal --lang <tag> <指令>`：只有這一個指令用另一種語言；`--lang en` 是英文。
- `wgal notify language <tag>`：只改訊息的語言。
- `wgal language --clear`：`wgal` 回到英文（訊息維持原來的語言）。退回 4.0.0 以前的版本之前要先做：較舊的 WatchDog 在通知設定裡有 `wgal` 的語言時讀不懂它，會讓通知停在關閉，直到設定修好。

指令、選項與值（`on`、`off`、`--yes`、隧道的 ID）在每種語言都一樣，照這裡寫的輸入。

### 把設定搬到另一台電腦

`wgal export > wgal-settings.json` 寫出設定檔（不含裝置名稱、Chat ID 與 Bot Token）。在另一台電腦，`wgal import --dry-run < wgal-settings.json` 顯示會改什麼，`wgal import --yes < wgal-settings.json` 套用；選項見 `wgal help import`。

Windows 上這兩個要在命令提示字元（`cmd.exe`）執行，不要用 PowerShell：PowerShell 沒有 `<`，Windows PowerShell 的 `>` 會把檔案寫成 UTF-16，`wgal import` 讀不了。

```bat
"C:\Program Files\WGAL\wgal.exe" export > wgal-settings.json
"C:\Program Files\WGAL\wgal.exe" import --dry-run < wgal-settings.json
"C:\Program Files\WGAL\wgal.exe" import --yes < wgal-settings.json
```

### 結束碼

0 完成；1 被拒絕，或完成但結果失敗；2 參數或輸入有誤（什麼都沒改）；3 連不到 WatchDog（什麼都沒改）；4 可能已經執行，但沒有確認。為了連錯誤也用選定的語言說明，沒有 `--lang` 時 `wgal` 會先讀一次 WatchDog 的狀態，這不會改變任何東西。加上 `--json` 時，標準輸出是一個 JSON 物件而不是文字；它的鍵與 `kind` 的值在每種語言都一樣。

## 讓 Agent 透過 MCP 使用 WGAL

`wgal-mcp` 讓 AI Agent 以和視窗相同的權限使用 WGAL，限於這個安裝的存取清單。它是 stdio 的 MCP 伺服器，由 Agent 啟動。使用者開啟之前，它是關閉的。

1. **使用者自己開啟 MCP**：在視窗的 MCP 頁，或在自己的終端機執行 `wgal mcp on` 並輸入 yes。Agent 絕不可以替使用者做這件事，也不可以替使用者回答那個提示。（不是在互動式終端機執行時，命令也會拒絕，什麼都不改；那只是確認的步驟，不能證明輸入的是人。）MCP 關閉時，`wgal-mcp` 什麼都不回答。
2. **把 `wgal-mcp` 加到 Agent 的 MCP 設定**：使用者確認後，視窗的 MCP 頁可以替 Claude Code、Claude Desktop、Cursor、VS Code、Trae、Kiro、Codex、Antigravity 與 Gemini CLI 加上。手動加的話，新增一個名為 `wgal` 的伺服器，以完整路徑執行 `wgal-mcp`，不帶參數：

   | 平台 | 路徑 |
   |---|---|
   | Windows | `C:\Program Files\WGAL\wgal-mcp.exe` |
   | Ubuntu | `/opt/wgal/wgal-mcp` |
   | macOS | `/Applications/WG Always Live.app/Contents/MacOS/wgal-mcp` |

3. **Agent 能做與不能做的事**：
   - 能做使用者在視窗裡能做的事，限於存取清單。
   - 永遠拿不到 Bot Token。
   - 使用者沒有另外開啟「讓 Agent 設定 Telegram」（`wgal mcp telegram on`，或同一頁）時，不能改裝置名稱、Chat ID 與 Bot Token，也讀不到 Chat ID。
   - 開啟 MCP 不等於允許開關隧道：連線、斷線、重連與開啟自動復原，只在使用者要求時才做。

工具清單在原始碼 repo 的 `docs/agents.md`，伺服器本身也會列出（`tools/list`）。

---

## 從原始碼建置發布包（在目標平台上）

發布包要在各自的平台上建置。Windows 無法替 macOS 或 Ubuntu 建置介面。

建置需要的工具：
- AGENTS.md「Toolchain」所列的版本：Rust 1.99.1、Node.js 24.16.0、pnpm 11.10.0；
- 平台套件：Ubuntu 要 `libwebkit2gtk-4.1-dev` 等套件，macOS 要 Xcode Command Line Tools。

工作樹必須乾淨。在 repo 根目錄執行：

| 平台 | 指令 | 產出 |
|---|---|---|
| Windows | `powershell.exe -NoProfile -ExecutionPolicy Bypass -File packaging\windows\make-release.ps1` | `dist\wgal-<版本>-windows-x64.zip` |
| Ubuntu | `packaging/linux/make-release.sh` | `dist/wgal-<版本>-linux-<arch>.tar.gz` |
| macOS | `packaging/macos/make-release.sh` | `dist/wgal-<版本>-macos-<arch>.zip` |

建置腳本只寫 `target/` 與 `dist/`，不安裝任何東西。建好之後，照上面各平台的步驟安裝。

## 腳本絕不做的事

- 不啟動、停止、修改或刪除任何官方 WireGuard 的隧道、服務、設定或開機自動連線設定。唯一的例外是 Windows 的緊急復原：`revert.cmd`／`revert.ps1 -Tunnel <名稱>` 會結束 WGAL 服務，並把列出、目前停著的隧道啟動回來（見 Windows 的 `README.zh-TW.md`「緊急復原」）。Agent 不要自行執行它，要先取得使用者對那幾條隧道的明確同意。
- 不讀 WireGuard 設定檔內容、私鑰或 Bot Token。
- 不改防火牆、路由、DNS、MTU，不安裝或移除其他套件。唯一的例外：Ubuntu 的 `install.sh` 在使用者同意時，安裝 WGAL 視窗需要、而系統缺少的 `libwebkit2gtk-4.1-0` 與 `libayatana-appindicator3-1`（不會移除它們）。
- 不改 PATH。Ubuntu 的 `install.sh` 只在 `/usr/local/bin/wgal` 沒有東西時建立連結（已經是 WGAL 的連結就保留），`uninstall.sh` 只移除這個連結。
- 只新增或移除 WGAL 自己的檔案、服務或 LaunchAgent，以及 WGAL 的資料。資料預設保留，移除時要明確要求才會刪除。

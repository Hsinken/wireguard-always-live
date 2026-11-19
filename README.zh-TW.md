# WG Always Live

[English](README.md) | **繁體中文**

WG Always Live（WGAL）替 Windows 11、macOS 與 Ubuntu（有桌面或伺服器）上的官方 WireGuard 隧道做背景監看。它確認每條隧道是否真的有流量，並顯示結果。只有在你允許時，它才重新連線出問題的那一條。自動復原失敗時，可以用 Telegram 通知你。

> **支援完整的 MCP 操作，建議用 AI Agent 安裝。**
> 安裝由 Agent 做，你負責授權。把這句話貼給你的 Agent：
>
> ```text
> 依照 https://github.com/Hsinken/wireguard-always-live 的 README 幫我安裝 WG Always Live
> ```
>
> 給 Agent：執行任何命令之前，先讀[用 AI Agent 安裝](#用-ai-agent-安裝)與[給 Agent 的規則](packaging/INSTALL.zh-TW.md#給-agent-的規則先讀)。

> **狀態：尚未正式發布。** 4.2.1 是第一個公開版本。Releases 頁面還沒有發布任何版本：目前請在要安裝的那台電腦上自己建置發布包。見[各平台狀態](#各平台狀態)。

## WGAL 是做什麼的

WGAL 是為了補強 WireGuard 官方程式對斷線判斷的不足而設計的。

- **問題**：在不穩定的網路上長時間運作之後，隧道可能顯示「已連線」，流量卻過不去。
- **檢查**：WGAL 經過隧道自己的網路介面，探測你指定的目標。只有探測有回應，這條隧道才算正常。
- **修復**：你允許時，WGAL 只重新連線出問題的那一條，其他隧道完全不碰。

先知道三件事：

- **WGAL 不是 VPN。** 它不取代 WireGuard。VPN 仍然是官方 WireGuard 程式（Ubuntu 上是 `wg-quick`）。
- **先裝 WireGuard。** 安裝 WGAL 之前，先安裝官方 WireGuard，並在其中設好隧道。
- **WGAL 不碰機密檔案。** 它從不讀取、修改或匯出 WireGuard 的設定檔或金鑰。

## 功能

- **逐條隧道的健康狀態**：探測綁定在隧道的網路介面上，送往你指定的 IPv4「復原判定目標」，以綠、黃、紅、灰燈號顯示，並附上原因。
- **可調整的故障判定**：每條隧道各自設定探測間隔、逾時、重試次數與最短故障時間。
- **自動復原，逐條開啟**：只重新啟動出問題的那一條，重試次數可設定。
- **Telegram 通知**：第一輪自動復原失敗時、重試次數用完時，以及通訊恢復時。
- **事件紀錄**（約 72 小時），以及紀錄匯出：IP 位址、端點、UUID、帳號路徑與 Chat ID 會遮蔽，不含 Bot Token。
- **暫停監看**：可以暫停全部，也可以只暫停一條。
- **設定檔**：匯出與匯入，用來備份，或搬到另一台電腦。
- **命令列**（`wgal`）：視窗對背景監控做的每一件事，都能在終端機裡做。沒有桌面的 Ubuntu 伺服器就用它來使用 WGAL。
- **MCP 伺服器**（`wgal-mcp`）：AI Agent 能以和視窗相同的權限使用 WGAL。它拿不到 Bot Token，而且你開啟之前是關閉的。
- **13 種語言**：視窗、命令列與 Telegram 訊息都可以用。
- 系統匣或選單列圖示。

## WGAL 絕不做的事

- 讀取、修改、匯出或刪除 WireGuard 的設定與金鑰。
- 修改路由、DNS、防火牆、MTU、AllowedIPs 或 On-Demand 規則。
- 改變隧道是否開機自動啟動。
- 啟動或停止你沒有交給它的隧道。登記隧道與開啟它的自動復原，是兩個分開、明確的選擇。
- 自行把資料送到任何地方：沒有遙測，也不檢查更新。它主動送出的流量只有你設定的探測，以及你設定好通知後，送往官方 Telegram Bot API 的訊息。

## 用 AI Agent 安裝

步驟由 Agent 執行，每一項權限由人給。

### 人要做的事

每一列是一項分開的權限。

| 步驟 | 人要做的事 |
|---|---|
| 在 Windows 或 Ubuntu 安裝 | 核准一次系統管理員權限（UAC，或 sudo 密碼），並從清單中挑選隧道。清單由 Agent 以不需要權限的命令取得 |
| 在 macOS 安裝 | 通常不用做什麼：以使用者自己的身分安裝，不用 sudo，也不挑隧道。只有使用者寫不進 `/Applications` 時，才會要求系統管理員密碼 |
| Telegram 通知（選用） | 自己輸入 Bot Token，在視窗裡，或用 `wgal notify token set` |
| MCP（選用） | 自己開啟 MCP：在視窗的 MCP 頁，或在自己的終端機執行 `wgal mcp on`，輸入 yes |
| 讓 Agent 設定 Telegram（選用） | 第二個開關，用同樣的方式開啟（`wgal mcp telegram on`） |

**同意安裝，不等於同意開關 VPN。** 只有人要求時，Agent 才連線、斷線或重連隧道，或開啟自動復原。

### 給 Agent 的規則

完整的規則在[給 Agent 的規則](packaging/INSTALL.zh-TW.md#給-agent-的規則先讀)，請先讀。摘要：

| 規則 | 意思 |
|---|---|
| 絕不經過要監看的 VPN 操作 | 在電腦本機操作，或經過不依賴那條 VPN 的連線。這條永遠適用 |
| 絕不讀取或印出秘密 | 包括 WireGuard 設定、私鑰與 Bot Token。絕不把 Token 放在命令列上。這條永遠適用 |
| 不改 WireGuard 的任何東西 | 不改設定、金鑰、隧道或開機啟動。不要手動連線或斷線 VPN |
| 不安裝其他東西 | 不裝其他套件，也不改防火牆、路由或 DNS |
| 先問，不要猜 | 問哪個帳號使用 WGAL、哪些隧道可以交給 WGAL，以及在 Ubuntu 上這台電腦有沒有桌面 |
| 只提升權限一次 | 先以不需權限的命令列出隧道，再以系統管理員權限執行一次安裝 |
| 檢查每個結束碼 | 只有 0 是成功。其他結果就停下來，把腳本的訊息原封不動交給人。不要自己「修好」再試 |

### Agent 的步驟

1. **確認 WireGuard 已就緒。** 官方 WireGuard 必須已經安裝，並有要監看的隧道：

   | 平台 | 需要 |
   |---|---|
   | Windows 11 | WireGuard for Windows。只有啟用過的隧道才能登記 |
   | macOS | Mac App Store 上的 WireGuard |
   | Ubuntu | `wireguard-tools`，每個設定放在 `/etc/wireguard/<名稱>.conf` |

   還沒有的話，就停下來告訴人。不要自己安裝或設定 WireGuard。
2. **取得壓縮包。** 從 [Releases 頁面](https://github.com/Hsinken/wireguard-always-live/releases)下載符合平台的壓縮包，用頁面上公布的 SHA-256 核對。**目前還沒有發布任何版本**：在那之前，照[自己建置發布包](#自己建置發布包)做。

   | 平台 | 壓縮包 |
   |---|---|
   | Windows 11（x64） | `wgal-<版本>-windows-x64.zip` |
   | macOS | `wgal-<版本>-macos-<arm64 或 x86_64>.zip` |
   | Ubuntu | `wgal-<版本>-linux-x86_64.tar.gz` |

3. **解開，用 `RELEASE.txt` 核對檔案。** `RELEASE.txt` 記載版本、建置用的 commit，以及包內每個檔案的 SHA-256。Windows 用 `Get-FileHash`，macOS 用 `shasum -a 256`，Ubuntu 用 `sha256sum`。這只證明內容與建置紀錄一致，不證明下載的檔案是真的。
4. **安裝**：照下面你的平台那一節，以及 [packaging/INSTALL.zh-TW.md](packaging/INSTALL.zh-TW.md) 的完整說明。壓縮包裡也有同一份指南。檢查每個結束碼。
5. **驗證** WatchDog 在執行，方法見同一節。
6. **人需要時，設定 MCP。** 見[讓 Agent 透過 MCP 使用 WGAL](#讓-agent-透過-mcp-使用-wgal) 與 [docs/agents.md](docs/agents.md)（英文）。

升級或移除之前，WGAL 視窗必須已經結束，不能有正在執行的 `wgal` 命令，使用 `wgal-mcp` 的 Agent 也要先關掉。

#### Windows 11

所有命令都在解開的資料夾裡執行。在 PowerShell 裡一定要保留 `.\`：只打 `install.cmd`，可能會執行 PATH 上另一個程式的腳本。

1. 從一般的 PowerShell 列出隧道。不需要系統管理員權限，印出一行 JSON，什麼都不改：

   ```powershell
   .\install.cmd -ListTunnels
   ```

   只會列出啟用過的隧道。`"accepted": false` 的隧道不能選。讓人從清單中挑選。
2. 以系統管理員身分安裝，例如用 `Start-Process -Verb RunAs` 啟動。人核准一次 UAC：

   ```bat
   .\install.cmd -Tunnel <隧道名稱>[,<隧道名稱>...]
   ```

   提升權限時用的是另一個管理員帳號的話，加上 `-User <使用 WGAL 的帳號的 SID>`。升級時不給 `-Tunnel`，存取清單維持原樣。
3. 以使用 WGAL 的帳號，從一般（未提權）的 PowerShell 驗證：

   ```powershell
   sc.exe query WGALWatchdog
   & 'C:\Program Files\WGAL\wgal.exe' --lang en status
   ```

   服務顯示 `RUNNING`，狀態的第一行以 `WGAL watchdog <版本>` 開頭。

細節：[packaging/windows/README.zh-TW.md](packaging/windows/README.zh-TW.md)。

#### Ubuntu

1. 問人這台電腦有沒有桌面。
2. 列出隧道。印出 JSON，什麼都不改：

   ```bash
   ./install.sh --list-tunnels
   ```

   一般使用者讀不到 `/etc/wireguard`，所以只會列出 systemd 知道的與正在執行的隧道（`"complete": false`）。要完整的清單，在接著安裝的同一個 sudo 工作階段執行 `sudo ./install.sh --list-tunnels`。讓人從清單中挑選。
3. 安裝。人輸入 sudo 密碼。

   有桌面：

   ```bash
   sudo ./install.sh --tunnel <介面>[,<介面>...]
   ```

   沒有桌面（伺服器）。每次升級都要再給 `--no-window`：

   ```bash
   sudo ./install.sh --no-window --tunnel <介面>[,<介面>...]
   ```

   使用 WGAL 的帳號是執行 sudo 的那個。要用另一個帳號，加上 `--user <帳號>`；不能是 root。

   有桌面時，視窗需要 Ubuntu 套件 `libwebkit2gtk-4.1-0` 與 `libayatana-appindicator3-1`。缺少時先問人，人同意才加上 `--install-runtime`。沒有加的話，腳本在改任何東西之前就會停下來。
4. 以使用 WGAL 的帳號驗證，不要用 sudo：

   ```bash
   systemctl is-active wgal-watchdog
   wgal --lang en status
   ```

   服務顯示 `active`，狀態的第一行以 `WGAL watchdog <版本>` 開頭。如果 `/usr/local/bin/wgal` 沒能建立連結，改用 `/opt/wgal/wgal`。

細節：[packaging/linux/README.zh-TW.md](packaging/linux/README.zh-TW.md)。

#### macOS

1. 以使用者自己的身分安裝，**不要用 sudo**。不需要挑隧道：

   ```bash
   ./install.sh
   ```

   腳本會把 app 複製到 `/Applications`。只有寫不進去時，才會要求系統管理員密碼。
2. 以使用者的身分驗證：

   ```bash
   "/Applications/WG Always Live.app/Contents/MacOS/wgal" --lang en status
   ```

   第一行以 `WGAL watchdog <版本>` 開頭。

請先讀[各平台狀態](#各平台狀態)：在 macOS 上，WGAL 目前還不會亮綠燈，也不能開啟自動復原。

細節：[packaging/macos/README.zh-TW.md](packaging/macos/README.zh-TW.md)。

### 安裝之後

人在視窗裡，或用 `wgal` 設定 WGAL。Agent 只在人要求時才執行這些命令。以使用 WGAL 的帳號執行 `wgal`，不要提升權限，也不要用 sudo。

| 平台 | 執行 `wgal` 的方式 |
|---|---|
| Windows | 在 PowerShell 執行 `& 'C:\Program Files\WGAL\wgal.exe' <命令>` |
| Ubuntu | `wgal <命令>`，或 `/opt/wgal/wgal <命令>` |
| macOS | `"/Applications/WG Always Live.app/Contents/MacOS/wgal" <命令>` |

| 命令 | 做什麼 |
|---|---|
| `wgal discover` | 列出 WGAL 能監看的隧道，以及 WGAL 給每條隧道的 ID |
| `wgal register <隧道>` | 開始監看一條隧道 |
| `wgal set <隧道> --recovery-target <位址>` | 設定探測必須經過那條隧道送達的目標。請用只有經過隧道才會回應的 IPv4 位址，例如對端網路上的主機（`192.0.2.1` 只是佔位） |
| `wgal probe <隧道>` | 立刻探測目標 |
| `wgal auto-recovery <隧道> on` | 讓 WGAL 在流量中斷時重新啟動這條隧道。只在人要求時才做 |
| `wgal status` | 顯示 WatchDog 與每條隧道 |
| `wgal help` | 列出所有命令 |

`wgal` 用人選的語言輸出。需要英文的輸出時加上 `--lang en`，需要一個 JSON 物件時加上 `--json`。人沒有要求時，不要改人的語言，也不要送測試訊息。

### 讓 Agent 透過 MCP 使用 WGAL

1. **人開啟 MCP。** 在視窗的 MCP 頁，或在自己的終端機執行 `wgal mcp on`，輸入 yes。Agent 絕不可以替人做這件事，也不可以替人回答那個提示。（不是在互動式終端機執行時，命令也會拒絕；那只是確認的步驟，不能證明輸入的是人。）
2. **把 `wgal-mcp` 加到 Agent 的 MCP 設定。** 人確認後，視窗的 MCP 頁可以替它支援的 AI 工具加上。否則，新增一個以完整路徑執行 `wgal-mcp` 的 stdio 伺服器：

   | 平台 | `wgal-mcp` |
   |---|---|
   | Windows | `C:\Program Files\WGAL\wgal-mcp.exe` |
   | Ubuntu | `/opt/wgal/wgal-mcp` |
   | macOS | `/Applications/WG Always Live.app/Contents/MacOS/wgal-mcp` |

3. **知道限制。** 透過 MCP，Agent 能做人在視窗裡能做的事，限於這個安裝的存取清單。它永遠拿不到 Bot Token。人沒有開啟第二個開關時，它不能改 Telegram 設定。

開啟 MCP 也不等於同意開關隧道。工具與它們的限制見 [docs/agents.md](docs/agents.md)（英文）。

## 自己安裝

發布包是以腳本安裝與移除的壓縮檔（`.zip`、`.tar.gz`）。沒有安裝程式，程式也還沒有程式碼簽章。Windows 可能會顯示 SmartScreen 警告。在 macOS 上，安裝腳本只對這個 app 移除下載隔離標記。

先安裝官方 WireGuard，並加好你的隧道。然後在解開的資料夾裡：

| 平台 | 簡單的方式 | 或是 |
|---|---|---|
| Windows 11 | 雙擊 `setup.cmd`。它要求一次系統管理員權限，讓你勾選隧道 | 以系統管理員身分執行 `.\install.cmd -Tunnel <隧道名稱>[,...]` |
| Ubuntu | `./setup.sh`（不要用 sudo）。它要求一次 sudo 密碼，讓你勾選隧道 | `sudo ./install.sh --tunnel <介面>[,...]` |
| 沒有桌面的 Ubuntu | `./setup.sh`，在問要不要不裝視窗時回答 yes | `sudo ./install.sh --no-window --tunnel <介面>[,...]` |
| macOS | 以你自己的身分執行 `./install.sh`，不要用 sudo | |

移除 WGAL：

| 平台 | 命令 |
|---|---|
| Windows 11 | 在解開的資料夾裡，以系統管理員身分執行 `.\uninstall.cmd` |
| Ubuntu | `sudo /opt/wgal/uninstall.sh` |
| macOS | `"$HOME/Library/Application Support/WGAL/uninstall.sh"`（要有引號，不要用 sudo） |

除非你要求刪除，WGAL 的資料都會保留。沒有任何腳本會改變 WireGuard 的隧道或設定。完整的指南是 [packaging/INSTALL.zh-TW.md](packaging/INSTALL.zh-TW.md)。

## 各平台狀態

以 2026-11-20 為準。

- **一個版號**：4.2.1 是第一個公開版本，也是視窗、WatchDog、`wgal-mcp` 與 `wgal` 第一次共用一個版號。之前的版本沒有公開。
- **還沒有發布**：Releases 頁面還沒有任何版本。請在要安裝的那台電腦上建置發布包，再用它安裝。

| 平台 | 狀態 |
|---|---|
| Windows 11 | 在開發者的電腦上，視窗 2.2.1 與 WatchDog 2.3.1（統一版號之前的版本）用在真實的隧道上。4.2.1 還沒有在 Windows 發布 |
| 沒有桌面的 Ubuntu（`--no-window`） | 4.2.1 安裝在一台測試伺服器上。那個建置是在原始碼搬到這個 repo 之前做的。見下表 |
| 有桌面的 Ubuntu | 2.2.1 曾安裝在一台有桌面（GNOME）的測試電腦上。4.2.1 還沒有裝在有桌面的電腦上。伺服器上的結果不代表桌面 |
| macOS | 原生建置在一台 Mac（macOS 27，Apple 晶片）上通過全部測試。2.2.1 的測試建置在那裡安裝、升級（刻意注入一次失敗）並移除過。4.2.1 裝在那台 Mac 上之後列不出隧道：macOS 27.2.1 把服務類型寫成冒號，它的解析器不接受。4.2.2（只發布 macOS）能讀；它在 Mac 上通過原生測試，發布包還沒有建置。控制官方 app 的 VPN 還沒有測試 |

在 Ubuntu 測試伺服器上，以 4.2.1：

| 已證明 | 未證明 |
|---|---|
| 綁定在隧道介面上的探測（ICMP；Ubuntu 不支援 TCP 探測） | 自動復原修不好的真實故障。能開啟自動復原，不代表它能修好故障 |
| 斷線、連線與重連，並確認通訊 | 兩條路由重疊的隧道 |
| 開啟自動復原 | |
| WatchDog 重新啟動與重開機之後，設定都保留 | |

**macOS 的限制**：綁定在隧道介面上的探測在 Mac 上證實之前，macOS 版不會亮綠燈，也不能開啟自動復原。這項能力是「未知」，不是「不支援」。

各平台證明了什麼、怎麼證明的，見 [docs/testing.md](docs/testing.md)（英文）。替身測試的結果，從不當成真實電腦上的結果回報。

## 運作方式

```text
 視窗（你的帳號，沒有特權） ────────────────┐
 wgal（命令列，你的帳號） ──────────────────┤
 wgal-mcp（AI Agent；預設關閉） ────────────┤
                                            │ 本機 named pipe／Unix socket
                                            ▼
                                     wgal-watchdog（背景監控）
                                     探測 · 復原 · 事件紀錄 · Telegram
                                            │
                                            ▼
                                     官方 WireGuard 的管理機制
```

| 平台 | 背景監控的執行身分 | 控制隧道的方式 | 本機連線 |
|---|---|---|---|
| Windows 11 | Windows 服務（LocalSystem） | `WireGuardTunnel$<名稱>` 服務 | 有存取控制清單的 named pipe |
| macOS | 你自己的 LaunchAgent | 以 `scutil --nc` 控制官方 app 的 VPN 設定 | `~/Library/Application Support/WGAL` 裡的 Unix socket |
| Ubuntu | systemd 服務（root） | `wg-quick@<介面>` 單元 | `/run/wgal/watchdog.sock` 的 Unix socket |

WatchDog 回答任何用戶端之前，都會先確認它在作業系統上的身分。在 Windows 與 Ubuntu 上，安裝時寫入的存取清單決定哪些帳號可以使用它，以及它們可以登記哪些隧道。

更多：[docs/architecture.md](docs/architecture.md)（英文）。

## 從原始碼建置

工具鏈（固定版本）：Rust 1.99.1、Node.js 24.16.0、pnpm 11.10.0。

| 平台 | 另外需要 |
|---|---|
| Windows | MSVC 建置工具與 WebView2 |
| Ubuntu | `libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev` |
| macOS | Xcode Command Line Tools |

CI 執行的檢查，在 repo 根目錄：

```bash
pnpm install --frozen-lockfile
pnpm run check                      # TypeScript、測試、建置
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

`pnpm tauri dev` 在本機執行視窗。沒有 WatchDog 時，它什麼都不監看。

### 自己建置發布包

每個發布包都在自己的平台上、在要安裝的那台電腦上建置。建置只寫入 `target/` 與 `dist/`，不安裝任何東西。

給 Agent：上面的建置工具不屬於 WGAL。缺少任何一個時，告訴人，先問過再安裝。

1. 取得原始碼。工作目錄不能有未提交的修改：

   ```bash
   git clone https://github.com/Hsinken/wireguard-always-live.git
   cd wireguard-always-live
   pnpm install --frozen-lockfile
   ```

2. 在 repo 根目錄建置：

   | 平台 | 命令 | 結果 |
   |---|---|---|
   | Windows | `powershell.exe -NoProfile -ExecutionPolicy Bypass -File packaging\windows\make-release.ps1` | `dist\wgal-<版本>-windows-x64.zip` |
   | Ubuntu | `packaging/linux/make-release.sh` | `dist/wgal-<版本>-linux-<架構>.tar.gz` |
   | macOS | `packaging/macos/make-release.sh` | `dist/wgal-<版本>-macos-<架構>.zip` |

3. 把 `dist/` 裡的壓縮包解到本機的資料夾，照 [Agent 的步驟](#agent-的步驟)從第 3 步開始安裝。

## 目錄結構

| 路徑 | 內容 |
|---|---|
| `apps/desktop/` | 視窗：Vite + TypeScript 的介面、它的 Tauri 2 外殼（`src-tauri/`）與語言檔（`src/lang/`） |
| `crates/wgal-protocol/` | 共用的契約：訊息、限制與 TypeScript 型別 |
| `crates/wgal-core/` | 與作業系統無關的狀態機 |
| `crates/wgal-platform/` | Windows、macOS 與 Ubuntu 的 adapter |
| `crates/wgal-runtime/` | WatchDog：服務的進入點、本機伺服器、儲存與通知 |
| `crates/wgal-client/` | 命令列與 MCP 伺服器共用的、連到 WatchDog 的方式 |
| `crates/wgal-cli/` | 命令列 `wgal` |
| `crates/wgal-mcp/` | MCP 伺服器 |
| `crates/wgal-testkit/` | 測試用的替身 |
| `packaging/` | 各平台的安裝、移除腳本與發布包建置工具 |
| `docs/` | 架構、測試、語言，以及給 Agent 的說明（英文） |

## 文件

| 檔案 | 用途 |
|---|---|
| [packaging/INSTALL.zh-TW.md](packaging/INSTALL.zh-TW.md)（[English](packaging/INSTALL.md)） | 安裝、升級、驗證與移除 WGAL，給人也給 Agent |
| [packaging/windows/README.zh-TW.md](packaging/windows/README.zh-TW.md)、[packaging/linux/README.zh-TW.md](packaging/linux/README.zh-TW.md)、[packaging/macos/README.zh-TW.md](packaging/macos/README.zh-TW.md) | 各平台的細節 |
| [docs/agents.md](docs/agents.md) | 給替人安裝 WGAL，或透過 MCP、`wgal` 使用它的 AI Agent（英文） |
| [docs/architecture.md](docs/architecture.md) | WGAL 的內部架構（英文） |
| [docs/testing.md](docs/testing.md) | WGAL 怎麼測試，以及各平台證明了什麼（英文） |
| [docs/languages.md](docs/languages.md) | 新增語言（英文） |
| [AGENTS.md](AGENTS.md) | 修改程式碼的規則（英文） |
| [CHANGELOG.md](CHANGELOG.md) | 每個版本改了什麼、發布給哪些平台（英文） |
| [SECURITY.md](SECURITY.md) | 回報漏洞（英文） |

## 語言

繁體中文、English、简体中文、日本語、한국어、Deutsch、Español、Português (Brasil)、Türkçe、Français、Русский、Bahasa Indonesia 與 Tiếng Việt。繁體中文與英文以外的翻譯是初稿。

| 部分 | 語言 |
|---|---|
| 視窗 | 跟隨系統語言，除非你自己選 |
| `wgal` | 你選擇之前是英文：`wgal language <tag>` 同時設定 `wgal` 與訊息，`wgal --lang <tag>` 只用在一個命令 |
| Telegram 訊息 | 你選擇之前是英文：`wgal language <tag>`、只設定訊息的 `wgal notify language <tag>`，或視窗的語言 |

新增語言時，視窗不用改程式，命令列要在它的檔案清單裡加一行：見 [docs/languages.md](docs/languages.md)（英文）。

## 參與開發

開發規則，包括每個修改都必須守住的安全邊界，寫在 [AGENTS.md](AGENTS.md)。預設執行的測試與 CI 從不碰真的 VPN：它們都以替身與 fixture 執行。少數的實機檢查預設略過，只有擁有者對那一次授權才會執行（[docs/testing.md](docs/testing.md)，英文）。Pull request 會在 Linux、Windows 與 macOS 上跑 CI（`.github/workflows/ci.yml`）。

回報漏洞請見 [SECURITY.md](SECURITY.md)。絕不要在 issue 裡貼 WireGuard 設定、金鑰或 Bot Token。

## 授權

[MIT](LICENSE)。

WireGuard 是 Jason A. Donenfeld 的註冊商標。WG Always Live 是獨立專案，與 WireGuard 及其作者沒有關聯，也未經其背書。

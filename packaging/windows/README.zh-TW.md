# WGAL Windows 安裝、升級、移除與緊急復原（WGW-001-D）

[English](README.md) | **繁體中文**

本資料夾的 PowerShell 腳本，由你在**以系統管理員身分開啟的 PowerShell** 中自行執行（`setup.ps1` 以雙擊 `setup.cmd` 啟動，自己要求系統管理員權限）。開發 WGAL 的 agent 不執行它們（它們會安裝或停止服務），只做語法檢查；代為安裝的 Agent 照 `INSTALL.zh-TW.md` 做。這是未簽章的開發測試版，沒有程式碼簽章或安裝程式套件；SmartScreen 可能提出警告。

| 腳本 | 用途 |
|---|---|
| `install.ps1` | 安裝；已安裝時為升級 |
| `uninstall.ps1` | 移除 |
| `revert.ps1` | 緊急復原：不靠 GUI 把 WGAL 停用並停止，把指定的隧道啟動回來 |
| `setup.ps1` | 勾選安裝：在小視窗列出隧道讓你勾選，再執行 `install.ps1`（以 `setup.cmd` 啟動） |

## 需要的環境

- Windows 11，已安裝官方 WireGuard for Windows，且要給 WGAL 監看的隧道已在官方 App 中存在（服務 `WireGuardTunnel$<名稱>`）。
- 建置：Rust 1.99.1（`rust-toolchain.toml`）、MSVC build tools、WebView2、Node.js 24.16.0、pnpm 11.10.0。

## 建置

在 repo 根目錄、乾淨的 commit 上執行，並記下 commit SHA：

```powershell
pnpm install --frozen-lockfile
cargo build --release --locked -p wgal-runtime --bin wgal-watchdog
cargo build --release --locked -p wgal-mcp
cargo build --release --locked -p wgal-cli
pnpm tauri build --no-bundle
```

`pnpm tauri build` 等於 `pnpm --filter @wgal/desktop run tauri build`，會先執行前端的 `pnpm run build`。四個執行檔都在 workspace 的 `target\release\`：`wgal-watchdog.exe`（背景服務）、`wgal-desktop.exe`（視窗）、`wgal-mcp.exe`（給 Agent 用的 MCP 伺服器，由 Agent 以 stdio 啟動；在視窗的「設定」或以 `wgal mcp on` 開啟 MCP 之前，它的要求一律被背景服務拒絕）與 `wgal.exe`（WGAL 的命令列，3.0.0 起，見下面「命令列 `wgal.exe`」）。`--no-bundle` 不產生 MSI 或 NSIS 安裝檔。GUI 只連線到 LocalSystem 擁有的 pipe（`connect_watchdog_pipe`，`3c7bc97` 起），`wgal-mcp.exe` 與 `wgal.exe` 也一樣。發布壓縮包由 `make-release.ps1` 建置，它以同樣的指令建置這四個執行檔。

## 放到同一個資料夾

```powershell
$stage = Join-Path $HOME 'wgal-stage'
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item target\release\wgal-watchdog.exe, target\release\wgal-desktop.exe, target\release\wgal-mcp.exe,
          target\release\wgal.exe, packaging\windows\install.ps1, packaging\windows\uninstall.ps1,
          packaging\windows\revert.ps1, packaging\windows\setup.ps1, packaging\windows\install.cmd,
          packaging\windows\uninstall.cmd, packaging\windows\revert.cmd, packaging\windows\setup.cmd $stage
```

## 開始之前

- **先退出 WGAL 視窗**：系統匣圖示的選單 →「退出介面（背景監控繼續）」。`install.ps1` 與 `uninstall.ps1` 在 `wgal-desktop`（視窗）、`wgal-mcp`（使用 WGAL MCP 的 Agent）或 `wgal`（命令列）還在執行時會拒絕執行（執行檔被佔用時無法替換或刪除）：先關閉那個 Agent 或它的 MCP 連線，等 `wgal` 指令結束或停止它，再執行一次。
- 在本機主控台、以系統管理員身分開啟 PowerShell 或「命令提示字元」。
- 每支腳本都以 `powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\<腳本>.ps1 ...` 執行；`-ExecutionPolicy Bypass` 只作用在這一個 PowerShell 程序，不改變系統設定。
- 在命令提示字元也可以直接執行同名的 `.cmd`（`.\install.cmd`、`.\uninstall.cmd`、`.\revert.cmd`，擁有者 2026-11-05 的需求）。在 PowerShell 裡執行 `.cmd` 時，前面的 `.\` 不能省：不加的話 PowerShell 會到 PATH 上找同名的檔案，可能執行到別的工具的腳本。這些 `.cmd` 都以系統內建的 Windows PowerShell（`%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe`，不從 PATH 找）照上面的方式執行同一支 `.ps1`，參數原樣傳入，結束碼也相同。
- 給 Agent 照著做的安裝與移除步驟，見 `packaging\INSTALL.md`（英文）與 `packaging\INSTALL.zh-TW.md`（發布壓縮包裡的 `INSTALL.md` 與 `INSTALL.zh-TW.md`）。

## 安裝

### 用勾選視窗安裝（建議給一般使用者）

在解壓縮後的資料夾裡雙擊 `setup.cmd`（擁有者 2026-11-12 的需求）：

1. Windows 詢問一次系統管理員權限（UAC）。之後的步驟都在同一個已提升權限的程序裡，不會再問。
2. 出現「WGAL 安裝」小視窗，列出這台電腦**啟用中**的 WireGuard tunnel（`install.ps1 -ListTunnels` 的結果），勾選要交給 WGAL 監看的那幾條，按「安裝」。沒有啟用的 tunnel 沒有服務，不會列出，請先在官方 WireGuard 程式啟用。WGAL 不接受的名稱會列出但不能勾。
3. 已經安裝過時，按鈕是「升級」，並預設勾選「維持目前的清單」：不改就照原樣升級（不給 `-Tunnel`）；取消勾選後重新勾 tunnel，會取代整份存取清單。
4. 視窗關閉後，在同一個黑色視窗執行 `install.ps1`，畫面與結束碼和直接執行時相同；最後按 Enter 關閉。

第一次安裝，以及重新勾選隧道時，雙擊 `setup.cmd` 的帳號就成為 WGAL 視窗的使用者：提升權限前先記下這個帳號的 SID，和勾選的隧道一起以 `-User` 傳給 `install.ps1`（用別的系統管理員帳號同意 UAC 時也一樣）。維持目前清單的升級兩者都不傳，存取清單保留它原本的帳號。WGAL 視窗、使用 WGAL MCP 的 Agent 還開著，或有 `wgal` 指令正在執行時，按「安裝」會提示先關閉或等它結束。

`setup.cmd -DryRun` 不需要系統管理員權限：顯示同一個視窗，最後只印出會執行的 `install.ps1` 指令，不執行、不改任何東西。

視窗在系統的顯示語言是中文時說繁體中文，其他語言說英文。

### 給 Agent：先列出 tunnel，再用一次系統管理員權限安裝

```powershell
.\install.cmd -ListTunnels
```

不需要系統管理員權限，不改任何東西，印出一行 JSON：`{"complete":false,"tunnels":[{"name":"office","status":"running","starts_at_boot":true,"accepted":true}]}`。`status` 是 tunnel 服務的狀態，`starts_at_boot` 是是否開機自動啟動，`accepted` 是 WGAL 是否接受這個名稱。`complete` 為 false，因為沒有啟用的 tunnel 沒有服務、不會列出。Agent 把清單給使用者勾選，再用**一次**系統管理員權限執行下面的安裝（例如以 `Start-Process -Verb RunAs` 啟動，使用者只需同意一次 UAC）。

### 直接執行

```powershell
cd $HOME\wgal-stage
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\install.ps1 -Tunnel '<隧道名稱>'
```

或在系統管理員的命令提示字元：

```bat
cd /d %USERPROFILE%\wgal-stage
.\install.cmd -Tunnel <隧道名稱>
```

參數：

- `-Tunnel`：允許使用者登記並交給自動復原的隧道。多條用逗號分隔成一個參數：`-Tunnel a,b`。`-File` 與 `.cmd` 都把它當成一個字串傳入，腳本會依逗號拆開（隧道名稱不會有逗號）。在 PowerShell 裡以 `-Command "& .\install.ps1 -Tunnel 'a','b'"` 給清單也可以。
  列在這裡的隧道，一般帳號的 GUI 也能開啟它的自動復原；只想監看、不能讓 WGAL 自動啟停的隧道，登記後保持自動復原關閉。名稱必須符合 WireGuard for Windows 的規則（`[A-Za-z0-9_=+.-]`，最多 32 字元，不是 `CON`、`NUL` 等），而且必須已有對應的 `WireGuardTunnel$<名稱>` 服務（唯讀查詢）。大小寫不必和官方 App 完全一樣：腳本找到服務後，以服務管理員中的拼法寫入存取清單（WGAL 比對名稱時分大小寫）。第一次安裝必填。
- `-User`：使用 WGAL 視窗的帳號 SID，預設是執行腳本的帳號。若提升權限時用的是另一個管理員帳號，請給實際使用視窗的帳號 SID（在那個帳號自己的一般視窗執行 `whoami /user`）。
- `-From`：放四個執行檔（`wgal-watchdog.exe`、`wgal-desktop.exe`、`wgal-mcp.exe`、`wgal.exe`）的資料夾，預設是腳本所在資料夾；缺任一個就拒絕。

腳本**先檢查、後動手**；任何一項檢查不通過就停止，什麼都不改：

1. 以系統管理員身分執行；WGAL 視窗、`wgal-mcp` 與 `wgal` 都沒有在執行；參數合法。
2. `C:\ProgramData\WGAL\` 若已存在：它和裡面的每一個項目都不是連結或 junction，擁有者都是 SYSTEM 或 Administrators（其他帳號擁有的項目可能是別人預先放進去的，請自行檢查並移除後再執行）。
3. 升級時，服務的執行檔路徑必須是本腳本安裝的路徑。

然後：

4. 記錄每個 `WireGuardTunnel$*` 與 `WireGuardManager` 服務的 Status 與 StartType。
5. 先處理資料夾，讓檢查與建立之間的空檔盡量短。第一次安裝時，以 `O:BAD:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)` 建立 `C:\ProgramData\WGAL\`：先在 ProgramData 下以隨機名稱建立**已帶有這個安全描述元**的新資料夾、檢查它，再改名為 `WGAL`；若這段時間有任何資料夾、檔案或 junction 出現在 `WGAL`，改名會失敗，腳本刪除自己的暫存資料夾並停止，不會寫進別人放的東西。改名後再檢查一次：不是連結、擁有者是 Administrators、DACL 不繼承且只有 SYSTEM 與 Administrators 完全控制。因此資料夾從不會有 ProgramData 預設「Users 可新增檔案」的那一刻。已存在時（升級）重新做上面「先檢查」第 2 項的檢查，並以同樣的條件檢查它的權限；權限被改過就停止，請自行檢查（腳本不重寫已存在資料夾的權限）。
6. 建立 `C:\Program Files\WGAL\`，把四個新執行檔先複製到它們旁邊、名稱加上 `.new`（例如 `wgal-watchdog.exe.new`），已在位的執行檔另外複製一份、名稱加上 `.old`；每份複本都以 SHA-256 核對。全部核對後把它們的雜湊記在旁邊的 `upgrade.record`（每個執行檔一行：名稱、新版雜湊、舊版雜湊或 `-`；最後一行 `started no`，見下面「升級」）。這兩種名稱與 `upgrade.record` 也和執行檔一樣檢查不是連結。之前的執行留下的同名檔案，先依它的 `upgrade.record` 確認在位的是那次記下的完整舊版或完整新版（或那次之前根本沒有安裝），才刪除並重新複製；沒有 `upgrade.record` 的複本是在準備途中就停下的那次留下的（那時在位的檔案還沒動過），直接刪除。無法確認時，腳本在停止服務之前就停下、什麼都不改，並說明怎麼做：那些複本可能是唯一完整的一組。那次可能已經啟動新版時（`upgrade.record` 不是 `started no`），只說明怎麼放回完整的新版，或原樣保留交給擁有者，不提放回舊版。
7. 再檢查一次資料夾的權限，把新的存取清單寫成**新檔** `access.txt.new`（UTF-8、無 BOM）：註解記錄由誰、何時寫入，然後一行 `user <SID>` 與每個 `tunnel <名稱>`；檔案擁有者設為 Administrators（服務只接受 SYSTEM 或 Administrators 擁有的存取清單）。之後以一次替換（`ReplaceFile`；還沒有舊檔時是改名）把它換成 `access.txt`：不先刪舊檔，所以任何時刻都有舊的或新的清單，任何人對舊檔持有的 handle 也碰不到新檔。
8. 建立服務 `WGALWatchdog`：`"C:\Program Files\WGAL\wgal-watchdog.exe"`，LocalSystem。第一次安裝時先建立為 **Disabled**，到第一次啟動之前（第 9 項）才改成 Automatic，所以在那之前失敗或按 Ctrl+C，留下的服務不會被重新開機或服務管理員啟動。升級時若它被 `revert.ps1`、或被一次沒有啟動它的執行（見下面）設成 Disabled，在停止服務之前改回 Automatic 並說明。復原動作（升級時也在停止之前設定）：失敗後 1 分鐘、1 分鐘、之後每 5 分鐘重新啟動，一天後重新計數（`sc.exe failure`）；並以 `sc.exe failureflag WGALWatchdog 1` 讓服務以錯誤碼自行停止時也觸發復原動作。第一次安裝時，換上存取清單與執行檔（`.new` 複製到原名並核對）後才建立服務。
9. 第一次安裝時，先把 `started yes` 寫進 `upgrade.record`、讀回確認，再把服務設成 Automatic；任何一步失敗就什麼都不啟動，把服務設回 Disabled（並讀回確認），保留複本與 `upgrade.record`，以 1 結束。啟動服務，最多等 30 秒到它進入 Running 且 `\\.\pipe\wgal-watchdog` 出現，然後像 WGAL 視窗一樣檢查這個 pipe 的擁有者必須是 SYSTEM：以 identification 層級連線（對方只能辨識、不能冒用這個管理員身分），讀取擁有者後不寫入任何東西就關閉。不是 SYSTEM 表示另一個程序佔用了這個名稱：服務照常監看與復原並持續重試，腳本大聲警告並以 3 結束，若 `service.log` 有 `the pipe could not be served` 也一併說明。啟動失敗時不會中斷：一樣印出 `sc.exe query`、結束碼的意義與 `service.log` 最後幾行。
10. 再記錄一次 WireGuard 服務並比對；有任何不同就大聲警告（腳本本身從不碰它們）。

結束碼：0 成功；1 服務沒有以 pipe 運作；2 WireGuard 服務在執行期間有變化；3 服務在執行，但它的 pipe 名稱被另一個程序佔用；4 升級在停止服務之後有一步失敗，腳本已用當時在位的完整一組執行檔把服務啟動回來（畫面說明是舊版還是新版，見「升級」）。只有 1、3、2 都不成立時才回 4。

安裝後，**從一般（未提權）的工作階段**，以檔案總管或「開始」開啟 `C:\Program Files\WGAL\wgal-desktop.exe`。不要從提升權限的視窗啟動它：那樣它以系統管理員身分執行，服務會把它當成已提權的管理員而繞過存取清單（GUI 絕不能以管理員身分執行）。腳本不建立捷徑、不設定登入時自動開啟，也不改 PATH，因為它不寫入那兩個資料夾以外的任何地方。

### 命令列 `wgal.exe`

3.0.0 起，每個發布包都帶 WGAL 的命令列 `wgal.exe`（擁有者 2026-11-18 的決定）；它用英文，4.0.0 起也能用 `wgal language <tag>` 選的 13 種語言之一，或以 `--lang <tag>` 只用在一個指令（擁有者 2026-11-19 的決定；`INSTALL.zh-TW.md` 的「語言」）。它和其他三個執行檔一起安裝、升級與移除，位置是 `C:\Program Files\WGAL\wgal.exe`。腳本不改 PATH，所以以完整路徑執行，例如在 PowerShell：

```powershell
& 'C:\Program Files\WGAL\wgal.exe' status
& 'C:\Program Files\WGAL\wgal.exe' --help
```

- 和視窗一樣，從**一般（未提權）**的終端機，以存取清單上的帳號執行。從提升權限的終端機執行時，它以系統管理員身分送出請求，不受存取清單對隧道的限制，所以平常不要那樣用。
- `wgal` 視為使用者本人（擁有者 2026-11-18 的決定）：任何以這個帳號執行的程式，包括有 shell 的 Agent，都能做這個使用者能做的事，包括讀 Chat ID、改 Telegram 設定。MCP 的兩個開關只限制 `wgal-mcp`。
- 它只送出和視窗相同的請求，不執行呼叫者給的命令、路徑或服務名稱；設定檔只從標準輸入讀、只寫到標準輸出。Bot Token 不從命令列或環境變數讀取：`wgal notify token set` 以不回顯的方式詢問。
- 4.1.0 起 WatchDog 在每則訊息發出前重讀系統的時區，但 Windows 給的可能是最多五分鐘前的時區（擁有者 2026-11-19 的決定）：時區剛改時的訊息可能還用舊的。`wgal host --time-zone <名稱>` 立即生效。

## 升級

已安裝時再執行同一個指令即為升級（先退出視窗）：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\install.ps1
```

先檢查 `ProgramData\WGAL` 與其中每一項。停止服務之前先做完能做的：新執行檔放成 `*.exe.new`、在位的執行檔另存 `*.exe.old`（上面第 6 項），給了 `-Tunnel` 時新清單寫成 `access.txt.new`（第 7 項），服務的啟動類型與復原動作（第 8 項）。然後停止 `WGALWatchdog`，等它停止（最多 60 秒），再等它的程序結束（最多再 30 秒）。接著在新版本第一次執行之前、沒有任何程式寫入的時候，**備份 WGAL 保存的狀態**：把 `state\state.json`、`state\state.prev.json` 與 `notify\notify.json`（存在的那幾個）照原本的子資料夾複製到新的資料夾 `C:\ProgramData\WGAL\backup\state-<升級當時的 UTC 時間，yyyyMMdd-HHmmss>Z\`，畫面印出完整路徑；資料夾與複本的擁有者設為 Administrators，權限繼承 `WGAL` 資料夾（只有 SYSTEM 與 Administrators）。只保留最新的 5 份，較舊的刪除（只刪這種名稱、只含這幾個檔案的資料夾；其他內容一律不動並警告）。備份不讀也不複製 `events\`、受保護的 Bot Token（`notify\` 中的 `.dpapi` 檔）、`access.txt` 與 `state.lock`。狀態檔或資料夾是連結、或同一秒已有備份時，備份失敗。然後換上 `access.txt.new`（一次替換），再把四個 `.new` 複製到原名並逐一核對。之後啟動服務並檢查 pipe 的擁有者（這也在受保護的範圍內，見下面），成功換上新的一組後刪除 `.new`、`.old` 與 `upgrade.record`。`access.txt` 只在給了 `-Tunnel` 或 `-User` 時重寫，而且是整份重寫（給了 `-User` 也必須給 `-Tunnel`）；其他資料（狀態、事件、通知設定、受保護的 Bot Token、`service.log`）全部保留。服務若被 `revert.ps1`、或被一次沒有啟動它的執行設成 Disabled，升級時改回 Automatic（這是 WGAL 自己的設定）並在畫面上說明。

**從停止服務到啟動後的檢查做完之前，任何一步失敗（包括按 Ctrl+C）**，腳本不會讓 WGAL 停在那裡，而是：

1. 以 SHA-256 判斷在位的是**完整的舊版**還是**完整的新版**（新版這時還沒執行過）。只換了一部分時，從 `*.exe.old` 把舊的複製回去，成為完整的舊版（舊版沒有的執行檔刪除）。
2. 啟動那一組並像平常一樣檢查（Running、pipe、擁有者），畫面以 `!!!` 開頭的警告說明在哪一步失敗、原因、現在跑的是舊版還是新版，以及存取清單：沒換上時「舊的清單仍然有效，這次給的沒有套用」；已換上時保留新的，**不會把舊清單放回去**（不會恢復你剛撤銷的授權）。
3. 不碰 WGAL 保存的狀態：舊版從來沒被新版碰過的狀態繼續執行，不會拿備份蓋回去。
4. 刪除 `.new` 與 `.old`，照常比對 WireGuard 服務。服務在執行時以 4 結束（1、3、2 優先），沒有執行時以 1 結束並說明現在沒有任何監看。

新版的啟動一開始，新版就可能已經執行過、改過狀態：這之後的失敗（例如啟動或檢查時按 Ctrl+C）**不會把舊版放回去**，腳本再啟動一次在位的完整新版，並說明新版可能已經執行過。

`upgrade.record` 的 `started` 一行記錄新版是否可能已經啟動過：準備時寫 `no`，開始啟動新版之前改成 `yes`；記錄只在完整的一組已啟動後才和複本一起刪除。之後的執行讀到的記錄不是 `started no`（或沒有這一行）時，就當成新版可能已經執行過、改過狀態：這次執行同樣**絕不啟動舊版**，也把 `started yes` 繼續記下去。

每次要啟動新版之前（第一次啟動，或復原時再啟動一次），腳本都先把 `started yes` 寫進 `upgrade.record`，再讀回來確認；確認不了（例如磁碟已滿，或別的程式開著這個檔案不讓寫）就**不啟動**服務，保留 `*.exe.old`、`*.exe.new` 與 `upgrade.record`，以 1 結束，並照實說明這次有沒有啟動過新版：把完整的新版留在原位、不要把 `*.exe.old` 複製回去，修正原因後再執行一次腳本；或原樣保留，交給擁有者判斷。

若兩組都湊不齊（例如舊檔也複製不回去），腳本**不啟動**服務、保留 `*.exe.old`、`*.exe.new` 與 `upgrade.record`，以 1 結束並說明。新版還沒啟動過時，之後再執行腳本會拒絕（不刪這些複本）：先修正原因，自己把完整的一組放回去（把每個 `*.exe.new` 複製到原名；或把每個 `*.exe.old` 複製到原名，並刪除沒有 `.old` 的執行檔），再執行一次。新版已經啟動、之後才失敗（例如沒有出現 pipe）時，腳本不會自動退回舊版：新版可能已經改過狀態，要退回請照「回退到較舊的版本」。

新版**可能已經執行過**（這次或之前某次已開始啟動新版）時，就算在位的是完整的舊版，腳本也不啟動它、不把舊版複製回去，保留複本與 `upgrade.record`，以 1 結束，並說明：**不要把舊版放回去（不要把 `*.exe.old` 複製回去）、不要啟動它**；先修正原因，再執行一次腳本，讓它換上完整的新版；在位的不是完整的一組時，再執行會拒絕，要先把每個 `*.exe.new` 複製到原名（各自要符合 `upgrade.record` 裡的新版雜湊）；或原樣保留，交給擁有者判斷。

上面三種**不啟動**的情況（記錄寫不進去、兩組都湊不齊、新版可能已經執行過而在位的不是完整新版），腳本在說明之前都先把 `WGALWatchdog` 的啟動類型設成 **Disabled** 並讀回確認，免得 Windows 在下次開機時自己啟動腳本剛拒絕啟動的那一組；只動 WGAL 自己的服務，不動任何 WireGuard 服務與它們的開機啟動。`!!!` 訊息會說已關閉自動啟動，下一次通過檢查的執行會在停止服務之前改回 Automatic（停止之前就拒絕的執行，例如上面說的「再執行會拒絕」，維持 Disabled）。設不成 Disabled 時，訊息直說關不掉、Windows 可能在下次開機時啟動在位的那一組：請在重新開機之前修正原因並再執行一次腳本，或原樣交給擁有者。

這段流程的測試是 `tests\install-recovery.tests.ps1`：以一般身分執行（`powershell.exe -NoProfile -ExecutionPolicy Bypass -File packaging\windows\tests\install-recovery.tests.ps1`），只載入 `install.ps1` 的函式，以替身取代所有服務指令，檔案只寫在 `%TEMP%` 下自己的資料夾，逐步注入失敗。替身只在測試裡，`install.ps1` 沒有選用它們的參數。它不是實機的升級測試。

**不要直接安裝比目前更舊的版本**，要回退就照下一節。較新的版本可能在狀態檔或通知設定檔多存欄位，較舊的版本讀不懂（例如 `9fe8123` 之前的版本讀不懂之後的狀態檔）：它會把狀態當成損壞而以唯讀模式執行，顯示儲存失敗，不做自動復原、不發通知，而且沒有其他明顯提示。

## 回退到較舊的版本

**退回 4.0.0 以前的版本**（擁有者 2026-11-19 的決定）：用過 `wgal language <tag>` 之後，`notify\notify.json` 存著 `wgal` 的語言，4.0.0 以前的 WatchDog 讀不懂這個檔：通知會停在關閉、也不接受通知設定的變更，直到檔案修好。退回之前，先在 4.0.0 以上執行 `wgal language --clear`（Telegram 訊息維持原來的語言），並確認 `wgal language` 顯示英文是預設。舊版本已經因此在通知關閉的狀態下執行時（退回時沒有做第 3a 步）：再安裝 4.0.0 以上，執行 `wgal language --clear`，然後照下面的步驟再退回一次。第 3 步會從備份放回 `notify.json`，而備份裡也可能有這個語言（從 4.0.0 以上升級時做的備份），所以退回 4.0.0 以前的版本時，第 3a 步會檢查；有這個語言時就刪掉這個檔，舊版本會以預設的通知設定啟動。不論如何，通知設定都不會比備份新；受保護的 Bot Token 不動。

要放回的是**升級到你要撤銷的那個版本時**所做的備份（升級時畫面印出的那個 `backup\state-…Z` 資料夾），不一定是最新的一份：之後若又升級或重裝過，會有更新的備份。回退會失去那次升級之後 WGAL 保存的變更（新的登記、監看與通知設定、事件與通知狀態、斷線與自動復原的選擇）；官方 WireGuard 的設定與服務不受影響，WGAL 的事件日誌與受保護的 Bot Token 也不動。

**舊版本必須把放回的狀態當成「從備份恢復」。** 備份停在升級的那一刻：那時還沒送出的通知，之後可能已由新版本送出；那時開著的自動復原，之後可能已被你關掉，隧道也可能已被你斷線。若把備份直接放回成目前的狀態檔，舊版本會照常信任它：可能再送一次同一則通知，或依過時的設定自動啟動你已斷線的隧道。所以第 3 步把備份放成**上一份**（`state\state.prev.json`），並把**目前的狀態檔**（`state\state.json`）改寫成一行刻意不是狀態檔的文字。WGAL 的狀態儲存遇到「目前的檔案壞了、上一份可讀」時，以保守的「從備份恢復」載入上一份：每條隧道的自動復原與「隧道被外部停止時也自動啟動」關閉、未完成的操作取消、尚未確定的通知改為「結果不明（不會自動重送）」，然後立刻把這個保守的狀態寫成新的目前檔案，上一份保持為備份。（這段邏輯在 `crates/wgal-runtime/src/store.rs` 與 `crates/wgal-core/src/watchdog.rs` 的 `distrust`；`00ad525`、`7339e8e` 與 1.0.0 的這兩段完全相同。）只放上一份而**沒有**目前的檔案不行：那會被當成上次儲存到一半，上一份照常被信任。

順序是先放回備份、再換回舊的執行檔，讓舊版本第一次啟動就讀到它看得懂的狀態。反過來做，舊版本會先讀到看不懂的檔案：以唯讀模式執行，或在只有上一份可讀時保守地恢復並寫回，放回備份前還得再停一次服務。

先退出 WGAL 視窗，然後在以系統管理員身分開啟的 PowerShell：

1. 找到那份備份：

   ```powershell
   $data = Join-Path ([Environment]::GetFolderPath('CommonApplicationData')) 'WGAL'
   Get-ChildItem -LiteralPath (Join-Path $data 'backup') -Directory | Sort-Object Name
   $from = Join-Path $data 'backup\state-<那次升級的時間>Z'
   if (-not (Test-Path -LiteralPath $from -PathType Container)) { throw "沒有這個備份：$from" }
   ```

2. 停止服務。`-NoWait` 只送出要求；第二行最多等 60 秒，服務到時還沒停止就以逾時錯誤結束。這時不要繼續：先執行 `revert.ps1`（緊急復原），它會結束服務的程序；等 `WGALWatchdog` 停止後再做第 3 步（第 3 步會再檢查一次）。

   ```powershell
   Stop-Service -Name WGALWatchdog -NoWait
   (Get-Service -Name WGALWatchdog).WaitForStatus('Stopped', [TimeSpan]::FromSeconds(60))
   ```

3. 放回備份。先檢查，任何一項不通過就停止、什麼都不改：服務已停止；`state` 資料夾存在；資料夾、備份與要動到的檔案都不是連結或 junction；要覆寫的路徑（`state\state.json`、`state\state.prev.json`、`notify\notify.json`）是檔案或不存在、不是資料夾，而且沒有別的名稱（hard link），寫入時不會改到別的檔；備份中若有狀態檔，至少一個讀得懂。整段以一個區塊執行，遇到第一個錯誤就停止（`&` 加上 `$ErrorActionPreference = 'Stop'`），寫入失敗之後的步驟都不會做；這時不要做第 4 步。然後：
   - 先把 `state\state.json` 改寫成標記文字。從這一刻起，中途被打斷留下的只會是保守的「從備份恢復」或唯讀，不會是被信任的舊狀態。
   - 備份中可用的狀態（它的 `state\state.json`；讀不懂時改用它的 `state\state.prev.json`）複製成 `state\state.prev.json`。備份中沒有任何狀態檔時（升級當時 WGAL 還沒存過狀態），兩個狀態檔都刪除，舊版本從空白開始。
   - 通知設定 `notify\notify.json` 照備份放回；備份中沒有的（是較新的版本才寫的）刪除。
   - 這幾個檔案的擁有者設回 Administrators。

   ```powershell
   & {
   $ErrorActionPreference = 'Stop'
   $state = Join-Path $data 'state'
   $current = Join-Path $state 'state.json'
   $previous = Join-Path $state 'state.prev.json'
   $settings = Join-Path $data 'notify\notify.json'
   $marker = 'WGAL rollback: not a state file. The watchdog loads state.prev.json as restored from a backup.'
   if ((Get-Service -Name WGALWatchdog).Status -ne 'Stopped') { throw 'WGALWatchdog 還沒停止，先做第 2 步。' }
   if (-not (Test-Path -LiteralPath $state -PathType Container)) { throw "沒有 $state，不是 WGAL 用過的資料夾。" }
   $paths = @($data, $state, (Join-Path $data 'notify'), $current, $previous, $settings, $from) +
       @('state', 'notify', 'state\state.json', 'state\state.prev.json', 'notify\notify.json' |
           ForEach-Object { Join-Path $from $_ })
   foreach ($path in $paths) {
       if ((Test-Path -LiteralPath $path) -and
           ((Get-Item -LiteralPath $path -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
           throw "$path 是連結或 junction，請自行檢查後再做。"
       }
   }
   foreach ($path in @($current, $previous, $settings)) {
       if (Test-Path -LiteralPath $path) {
           $item = Get-Item -LiteralPath $path -Force
           if ($item.PSIsContainer -or ($item.LinkType -eq 'HardLink')) {
               throw "$path 是資料夾或還有別的名稱（hard link），請自行檢查後再做。"
           }
       }
   }
   $found = @('state\state.json', 'state\state.prev.json' | ForEach-Object { Join-Path $from $_ } |
       Where-Object { Test-Path -LiteralPath $_ -PathType Leaf })
   $saved = $found | Where-Object {
       try {
           $json = Get-Content -LiteralPath $_ -Raw -Encoding UTF8 | ConvertFrom-Json
           $null -ne $json -and ($json.PSObject.Properties.Name -contains 'schema_version')
       } catch { $false }
   } | Select-Object -First 1
   if (($found.Count -gt 0) -and ($null -eq $saved)) { throw "備份中的狀態檔都讀不懂：$from" }
   [IO.File]::WriteAllText($current, $marker)
   if ($null -ne $saved) {
       Copy-Item -LiteralPath $saved -Destination $previous -Force
   } else {
       if (Test-Path -LiteralPath $previous) { Remove-Item -LiteralPath $previous }
       Remove-Item -LiteralPath $current
   }
   $backupSettings = Join-Path $from 'notify\notify.json'
   if (Test-Path -LiteralPath $backupSettings -PathType Leaf) {
       Copy-Item -LiteralPath $backupSettings -Destination $settings -Force
   } elseif (Test-Path -LiteralPath $settings -PathType Leaf) {
       Remove-Item -LiteralPath $settings
   }
   foreach ($path in @($current, $previous, $settings)) {
       if (Test-Path -LiteralPath $path -PathType Leaf) {
           $acl = Get-Acl -LiteralPath $path
           $acl.SetOwner((New-Object Security.Principal.SecurityIdentifier('S-1-5-32-544')))
           Set-Acl -LiteralPath $path -AclObject $acl
       }
   }
   }
   ```

3a. **只在退回 4.0.0 以前的版本時**，緊接著第 3 步、服務仍然停止時：檢查放回的通知設定裡有沒有 `wgal` 的語言（擁有者 2026-11-19 的決定）。從 4.0.0 以上升級時做的備份可能有它（用過 `wgal language <tag>` 之後），而 4.0.0 以前的版本讀不懂這樣的檔，會讓通知停在關閉。從那個舊版本本身升級時做的備份一定沒有，選那一份時這一步什麼都找不到。
   - 它只以 JSON 讀這個檔。頂層有 `cli_language` 時（不論值是什麼）就刪掉這個檔，和備份沒有這個檔時第 3 步的做法相同：舊版本會以通知關閉、它自己的預設值啟動，你再到它的視窗重新設定裝置名稱、Chat ID 與訊息的語言。受保護的 Bot Token 不動。
   - 這個檔是連結或 junction、不是 JSON 物件，或讀不了、刪不了時，它以錯誤停止、什麼都不改：它和第 3 步一樣以遇到第一個錯誤就停止的區塊執行，所以讀取或解析失敗就停止，不會拿前一次執行讀到的結果繼續判斷或刪除。這時不要做第 4 步。

   ```powershell
   & {
   $ErrorActionPreference = 'Stop'
   $settings = Join-Path $data 'notify\notify.json'
   if (Test-Path -LiteralPath $settings -PathType Leaf) {
       if ((Get-Item -LiteralPath $settings -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) {
           throw "$settings 是連結或 junction，請自行檢查後再做。"
       }
       Add-Type -AssemblyName System.Web.Extensions
       $object = (New-Object Web.Script.Serialization.JavaScriptSerializer).DeserializeObject([IO.File]::ReadAllText($settings))
       if ($object -isnot [Collections.Generic.IDictionary[string, object]]) { throw "$settings 不是 JSON 物件，請自行檢查後再做。" }
       if ($object.ContainsKey('cli_language')) {
           Remove-Item -LiteralPath $settings -ErrorAction Stop
           "已刪除 $settings（它存著 wgal 的語言）：請在舊版本重新設定通知。"
       }
   }
   }
   ```

4. 在**舊版本的資料夾**（舊版的執行檔與它自己的腳本）執行它的 `install.ps1`，不帶 `-Tunnel`：它替換回舊的執行檔並啟動服務（服務已停止也照常進行）。會備份的舊版 `install.ps1` 這時也會把第 3 步的結果另存一份備份（其中的 `state.json` 是標記文字）；之後要回退時，仍選升級當時的那一份。回退到 3.0.0 之前的版本時，舊版的 `install.ps1` 只認得它自己的三個執行檔：3.0.0 的 `wgal.exe` 留在 `C:\Program Files\WGAL\`，不會被替換或刪除（2.2.2 的 `uninstall.ps1` 移除整個資料夾時才一起刪除）。

   ```powershell
   cd <舊版本的資料夾>
   powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\install.ps1
   ```

5. 確認：`C:\ProgramData\WGAL\service.log` 最後一次啟動有 `the current state file was damaged; the previous copy was loaded`（這是預期的：第 3 步刻意讓舊版本這樣載入備份；備份中沒有狀態時則是 `no stored state; starting empty`）與 `running`，沒有 `stored state could not be loaded`；`state\state.json` 已不再是標記文字（舊版本寫入了保守的狀態）。以一般身分開啟的 WGAL 視窗顯示升級當時的隧道與設定，沒有儲存失敗。通知設定與升級前相同；但第 3a 步刪掉了 `notify\notify.json`、或備份本來就沒有這個檔時，通知是關閉的、用的是舊版本的預設值：請打開通知，並重新設定裝置名稱、Chat ID 與訊息的語言。

6. **回退後由你重新決定的事**（WGAL 不會自己恢復）：
   - 每條隧道的「自動復原」與「隧道被外部停止時也自動啟動」都已關閉，要的話在隧道卡片上重新開啟。關閉期間 WGAL 照常監看與顯示燈號，但不會自動啟動或停止任何隧道。
   - 連線意圖是升級當時的。升級後才由你斷線的隧道，舊版本會看到它停著，但分辨不出是誰停的（自動復原已關閉，啟動前也沒看過它連線）：卡片顯示灰燈「未啟用」、原因「未連線」，不是「外部停止」，也不會啟動它；要保持斷線就在 WGAL 按一次斷線，要連線就按連線。
   - 升級當時還沒結案、通知也還沒確定送出或取消的事件，通知狀態改為「結果不明（不會自動重送）」：WGAL 不會再送它，需要的話自行查看 Telegram 是否收到過。
   - 升級之後才做的登記、監看與探測設定、每條隧道的通知開關、暫停監看都已失去，要的話重新設定。

若沒有那次升級的備份（例如從不會備份的舊版 `install.ps1` 升級），唯一乾淨的回退是先以 `uninstall.ps1 -RemoveData` 移除（會刪除 WGAL 的狀態、事件、設定與 Token），再安裝舊版。

## 移除

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\uninstall.ps1             # 保留資料
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\uninstall.ps1 -RemoveData # 一併移除 ProgramData\WGAL
```

依序（先退出視窗）：記錄 WireGuard 服務；停止 `WGALWatchdog` 並等它與它的程序結束（若它正在重新啟動某條隧道，會先把那條隧道啟動回來再停止）；以 `sc.exe delete` 刪除服務；移除 `C:\Program Files\WGAL\`；只有給 `-RemoveData` 才移除 `C:\ProgramData\WGAL\`（包括 `backup\` 中升級時的狀態備份）。資料夾本身或其中任何項目是連結或 junction 時拒絕刪除，避免 Windows PowerShell 的遞迴刪除跟著連結刪到資料夾外。**即使前面某一步失敗**，最後仍會比對 WireGuard 服務；任何仍為 Stopped 的 `WireGuardTunnel$*` 只印出警告與你可以自行執行的指令 `Start-Service 'WireGuardTunnel$<名稱>'`，腳本不會啟動它。服務停止時卡住，先執行 `revert.ps1`，再重跑 `uninstall.ps1`。結束碼：0 成功；1 某一步失敗；2 WireGuard 服務有變化。

## 緊急復原（不靠 GUI）

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\revert.ps1 -Tunnel '<隧道名稱>'
```

只動 `WGALWatchdog` 與所指定隧道的「啟動」：

1. 把 `WGALWatchdog` 設為 Disabled（之後重新開機或 SCM 的復原動作都不會啟動它）。
2. 要求它停止但不等待，最多等 45 秒（它若還在啟動中而沒接受要求，等待期間會再要求）；仍未停止，就結束 SCM 回報給這個服務的那個程序，前提是服務登記的程式確實是 `"C:\Program Files\WGAL\wgal-watchdog.exe"`。
3. 每條 `-Tunnel`：等它的服務離開切換中的狀態（最多 60 秒），若為 Stopped 就要求啟動並最多等 60 秒到 Running；超過時明確警告，不會無限等待。
4. 印出 `WGALWatchdog` 與所有 WireGuard 服務的狀態與啟動類型。WGAL 已停止且隧道 Running 時結束碼為 0，否則為 1。

它從不停止或修改隧道服務、從不改變隧道的啟動類型、不碰 `WireGuardManager`，也不動 WGAL 保存的資料（自動復原的設定保持原樣）。恢復 WGAL：

```powershell
Set-Service -Name WGALWatchdog -StartupType Automatic
Start-Service -Name WGALWatchdog
```

升級時 `install.ps1` 也會把它改回 Automatic。自動復原維持原本的設定：WGAL 恢復執行後，開著自動復原的已登記隧道，只要達到它自己的故障確認條件（探測間隔、重試次數與最短故障時間；重試 0 次、最短故障時間 0 時，一輪失敗就夠），WGAL 就可能停止並啟動它；`wgal show <隧道>` 會顯示這些設定。若不希望 WGAL 馬上動作：在執行 `revert.ps1` 之前、視窗或 `wgal` 還連得到背景服務時，先暫停全部監看（視窗的「設定」頁，或 `wgal pause`）。暫停會被保存，所以 WGAL 恢復後仍是暫停：在你恢復監看之前不探測、不做任何自動動作。沒有先暫停時，WGAL 停止期間沒有辦法關閉自動復原。

## 疑難排解

- **視窗顯示「目前登入的帳號不在本機安裝的存取清單中…」**，或背景監控拒絕了這個帳號（有顯示細節時，開頭是 `unauthorized (caller_not_allowed)`）：這個帳號不在存取清單上，或服務拒絕了 `access.txt`。查看 `C:\ProgramData\WGAL\service.log` 是否有 `access list refused`（後面寫著行號與原因）；用正確的帳號重跑 `install.ps1 -Tunnel <名稱> -User <SID>`。同時查看是否有 `the pipe could not be served`：佔用 pipe 名稱的程序若拒絕這個帳號，視窗也會顯示同樣的「不在存取清單」訊息。
- **視窗顯示背景監控失聯，但服務是 Running**：查看 `service.log` 是否有 `the pipe could not be served`。這表示另一個程序佔用了 `\\.\pipe\wgal-watchdog`；服務照常監看與復原，並每分鐘重試一次，直到名稱空出來。視窗只連線到 LocalSystem 擁有的 pipe，所以不會把請求或 Token 送給佔用者。
- **服務沒有啟動**：`sc.exe query WGALWatchdog` 的 `SERVICE_EXIT_CODE`，見下表。

## 腳本會改什麼、絕不改什麼

| 會建立或修改 | 絕不修改 |
|---|---|
| `C:\Program Files\WGAL\`（四個執行檔；安裝與升級期間另有它們的 `*.exe.new`、`*.exe.old`，完成後刪除） | 任何 `WireGuardTunnel$*` 服務的設定、ACL 與啟動類型（`revert.ps1` 只會啟動已停止的指定隧道） |
| `C:\ProgramData\WGAL\`（擁有者、DACL、`access.txt` 與寫入前的 `access.txt.new`；升級時在 `backup\` 新增狀態的備份並刪除第 5 份以前的） | `WireGuardManager` 與官方 WireGuard App |
| 服務 `WGALWatchdog`（建立、復原動作、啟動、停止、停用、刪除） | WireGuard 設定檔、金鑰、路由、DNS、防火牆、MTU、網路介面 |
| | 登錄的啟動項、排程工作、捷徑、其他程式 |

## 服務的檔案與結束碼

`C:\ProgramData\WGAL\` 內：`access.txt`（安裝程式寫）、`state\`（狀態）、`notify\`（通知設定與 DPAPI 保護的 Token，只有 LocalSystem 能解開）、`events\`（`events.jsonl` 事件日誌）、`backup\`（升級時的狀態備份，安裝程式寫，最多 5 份，服務不讀）、`service.log`（服務自己的啟動、停止、pipe 重試、網路變更與錯誤紀錄，最多 256 KiB，保留一份舊檔）。

`sc.exe query WGALWatchdog` 的 `SERVICE_EXIT_CODE`（`WIN32_EXIT_CODE` 為 1066）：

| 碼 | 意義 | 留下的紀錄 |
|---|---|---|
| 1 | 找不到 ProgramData，或 `WGAL` 資料夾不存在、是連結、擁有者或 DACL 不安全 | 無（不信任的資料夾不寫入任何東西） |
| 2 | `state`、`notify` 或 `events` 子資料夾無法建立或不安全 | `service.log` |
| 3 | 狀態儲存無法開啟（例如另一個 watchdog 持有鎖） | `service.log` |
| 4 | watchdog 迴圈無法啟動 | `service.log` |
| 11 | 執行中 watchdog 迴圈連續三次（約 90 秒）沒有回應 | `service.log` |

以上錯誤碼都會觸發 SCM 的復原動作。pipe 的問題不會讓服務停止（見疑難排解）。由你（或關機）要求的停止一律以成功結束，所以 `Stop-Service WGALWatchdog` 之後不會被自動重新啟動；停止時若迴圈卡住，服務最多約 45 秒後回報已停止並結束程序。

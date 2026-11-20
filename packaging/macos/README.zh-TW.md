# WGAL 在 macOS 上的安裝、升級與移除

[English](README.md) | **繁體中文**

安裝、驗證與移除的步驟見 `INSTALL.zh-TW.md`；這份文件說明腳本做了什麼、檔案在哪裡、出問題時怎麼看。

> **狀態（2026-11-20）**：
> - 2026-11-13 第一次在一台 Mac（Apple 晶片，macOS 27.2.1）上原生建置與測試：五個命令通過，腳本的替身測試也以 macOS 內建的 bash 3.2 執行並通過。
> - 同一天以一個測試用的建置（介面 2.2.1、WatchDog 2.3.1 的版本號）在那台 Mac 上實機安裝、升級（其中一次在停止 WatchDog 之後注入失敗）與 `uninstall.sh --remove-data`：PASS。這不是「已在裝置上驗收」。
> - 以從這個 repo 建置的 4.2.1，那台 Mac 上的 WatchDog 列不出隧道（`manager_query_failed`）：macOS 27.2.1 把服務類型寫成 `[VPN:com.wireguard.macos]`，用冒號，而解析器只接受斜線。4.2.2 兩種都能讀。清單與 status 的格式是在那台 Mac 上唯讀讀取的，沒有改任何東西。
> - 以 4.2.2（2026-11-20），那台 Mac 上的 WatchDog 列得出隧道，也讀到已登記隧道的狀態是連線中。
> - 同一天在那裡以 4.2.2 測了綁定探測，經它以 LaunchAgent 執行的 WatchDog，用那條連線中的隧道（[docs/testing.md](../../docs/testing.md) 的 M1 到 M6）：停止與啟動前後，目標都經隧道回覆；隧道路由內一個靜默的位址沒有回覆；區網閘道與 Mac 在區網上的位址在送出之前就被拒絕；WatchDog 在 30 秒內停止並啟動隧道，沒有跳出提示，停止它的 SSH 連線結束之後也照常動作：PASS。有一項沒有通過：隧道自己的位址沒有被拒絕（它的路由帶 `LOCAL` 旗標），送出了一個 echo，沒有回覆。
> - 4.3.0 拒絕這種路由，並依擁有者 2026-11-20 的決定採信綁定探測：復原判定目標經隧道回覆之後，燈號可以變綠，也能開啟自動復原。
> - 4.3.0（2026-11-20）在那台 Mac 上，經它的 WatchDog、用同一條隧道再通過同樣的檢查：燈號變綠；區網閘道、Mac 在區網上的位址與隧道自己的位址都在送出之前就被拒絕，靜默的位址沒有回覆；經 `wgal` 中斷、連線與重新連線都以結束碼 0 完成，中斷之後一直保持斷線到下一次連線，連線與重新連線之後各有一次新的、驗證過的探測與綠燈；開啟自動復原被接受。
> - 實機上還沒有驗證的（NOT RUN）：鎖定螢幕後讀取鑰匙圈、同時兩條隧道。
> - 需要自動復原修好的故障：BLOCKED，沒有已授權的方法讓目標不回應。
> - 仍有的限制：Network Extension 停擺而 `scutil --nc status` 仍顯示 `Connected` 時，`ping` 可能印出 WGAL 還不認得的 `sendto` 錯誤。這會判為「探測不可用」，不是失敗，所以不會觸發自動復原，也不會發通知；要等 Mac 上實際看到它印出什麼才改。
> - 實機上也還沒有測的（NOT RUN）：把舊 app 改名回來的復原、「可能已執行」之後的拒絕啟動、登入時啟動的關閉與重新開啟、Ctrl-C 中斷、需要 sudo 才能寫 `/Applications` 的情況。

## 運作方式

- **WatchDog**：使用者自己的 LaunchAgent（規格第 3.1 節）。登入後由 launchd 以使用者身分啟動，只有這個使用者的權限，沒有 root helper。它以 `/usr/sbin/scutil --nc` 控制官方 WireGuard app 的 VPN 設定，並以 `/sbin/ping`、`/sbin/route` 探測。它從不修改 VPN 設定或 On-Demand 規則。
- **視窗**：經 `~/Library/Application Support/WGAL/watchdog.sock` 和 WatchDog 通訊，只連同一個使用者擁有的 socket。
- **命令列 `wgal`**：WGAL 的命令列，用英文或 `wgal language <tag>` 選的語言（4.0.0 起），在 app 裡的 `Contents/MacOS/wgal`。它和視窗一樣只連同一個使用者擁有的 socket、只送出請求。
  - 安裝腳本不改 PATH，所以以完整路徑、以自己的身分（**不要用 sudo**）執行：`"/Applications/WG Always Live.app/Contents/MacOS/wgal" status`。`--help` 列出所有命令。
  - `wgal` 視為你本人（擁有者 2026-11-18 的決定）：任何以你的帳號執行它的程式，包括有 shell 的 Agent，都能做你能做的事，包括讀 Chat ID、改 Telegram 設定。MCP 的開關只限制 `wgal-mcp`。
- **WatchDog 接受的對象**：只有同一個使用者，其他使用者的請求會被拒絕。因為 WatchDog 的權限和使用者自己相同，所以 macOS 沒有存取清單。
- **秘密**：Bot Token 存在使用者的登入鑰匙圈：服務 `io.github.hsinken.wg-always-live`，帳號 `telegram-bot-token`。Token 由視窗或 `wgal notify token set` 設定，`wgal` 不從命令列或環境變數讀 Token。WatchDog 讀取時不會跳出任何對話框。`uninstall.sh --remove-data` 以 app 內的 `wgal-watchdog --forget-token` 刪除它：只有確定刪除或本來就沒有時才算成功，鑰匙圈拒絕時會明說 Token 還在。

## 檔案

| 路徑 | 內容 |
|---|---|
| `/Applications/WG Always Live.app` | 介面；`Contents/MacOS/` 裡還有 `wgal-watchdog`、`wgal-mcp` 與命令列 `wgal` |
| `~/Library/LaunchAgents/io.github.hsinken.wg-always-live.watchdog.plist` | WatchDog（不是正常結束時，launchd 會重新啟動它） |
| `~/Library/LaunchAgents/io.github.hsinken.wg-always-live.window.plist` | 登入時開啟視窗 |
| `~/Library/Application Support/WGAL/`（0700） | `state`、`notify`、`events`、`watchdog.sock`（0600）、`uninstall.sh`；升級前的狀態備份 `backup/state-<UTC 時間>`（最多 5 份）；安裝期間的 `upgrade/`（新的 LaunchAgent 與舊的複本，完成後刪除） |
| `~/Library/Logs/WGAL/watchdog.log` | WatchDog 的紀錄 |
| `/Applications/.WG Always Live.app.new`、`.WG Always Live.app.old`（隱藏） | 只在安裝期間：複製好的新 app、換下來的舊 app；完成後刪除。升級沒有完成而留下時，`uninstall.sh` 連同 `upgrade/` 一起刪除（是連結時不動） |

## 升級與中途失敗

`install.sh` 每次都走同一條路（第一次安裝時「停止」與「備份」沒有東西可做）。它先確認發布包的 app 裡有四個程式（`wgal-desktop`、`wgal-watchdog`、`wgal-mcp`、`wgal`），缺任一個就在改任何東西之前停下。停止任何東西之前先做完能做的：資料與紀錄資料夾、`uninstall.sh`；先為發布包裡的 app 做一份**內容記錄**（每個資料夾、連結，與每個檔案的 SHA-256），存成 `upgrade/app.record`（列舉、讀連結或算雜湊有任何一項失敗，就沒有記錄：腳本以 65 停下，什麼都不停止、不刪除）；新 app 以 `ditto` 複製成隱藏的 `/Applications/.WG Always Live.app.new`，和這份記錄逐項比對，並移除它的下載隔離標記（需要時這時就會問管理員密碼）；兩份 LaunchAgent 寫在 `upgrade/` 並以 `plutil -lint` 檢查，在位的兩份另外複製一份。然後停止 WatchDog 與視窗的 LaunchAgent，**備份狀態**（和 Windows、Ubuntu 一樣：`state/state.json`、`state/state.prev.json`、`notify/notify.json` 複製到 `backup/state-<UTC 時間>`，只保留最新 5 份；Bot Token 在鑰匙圈，不在備份裡），把在位的 app 改名成 `.WG Always Live.app.old`、新的改名到原位（各一次改名，不再先刪舊 app），把 LaunchAgent 移到 `~/Library/LaunchAgents`，最後啟動並檢查 socket；成功後刪除 `.new`、`.old` 與 `upgrade/`（舊 app 刪不掉時保留 `upgrade/`，下次執行要用它的記錄）。

之前的執行留下隱藏的 `.WG Always Live.app.old`（它換下來的舊 app）時，只有在位的 app 和那次的 `upgrade/app.record` 完全相同（那次的完整新 app）才會刪除它；否則腳本在停止任何東西、改任何東西之前就以 65 停下，說明怎麼做：那次還沒啟動新版時，說明怎麼把舊 app 改名回來（那可能是唯一完整的 app）；那次可能已經啟動新版時（見下面的 `upgrade/started`），只說明怎麼放回完整的新 app，或原樣保留交給擁有者。

`upgrade/started` 記錄新 WatchDog 是否可能已經啟動過：準備時寫 `no`，開始啟動新版之前改成 `yes`；`upgrade/` 只在完整的 app 已啟動後才刪除。之後的執行找到 `upgrade/`，而這個檔案不是 `no`（或不存在、讀不出來）時，就當成新版可能已經執行過、改過狀態：這次執行同樣**絕不啟動舊 app**，也把 `yes` 繼續記下去。之後的執行**不會先刪掉整個 `upgrade/` 再重建**：它保留這個資料夾，先以一次改名寫好 `started`（之前是 `yes` 就仍是 `yes`），再處理其他準備內容，所以在這中間失敗或被中斷，下一次執行仍會知道新版可能已經執行過。

每次要啟動新 WatchDog 之前（第一次啟動，或 trap 復原時再啟動一次），腳本都先把 `yes` 寫進 `upgrade/started`，再讀回來確認；確認不了（例如磁碟已滿）就**不啟動**，保留隱藏的 `.WG Always Live.app.old` 與 `upgrade/`，以 1 結束，並照實說明這次有沒有啟動過新 WatchDog：把完整的新 app 留在原位、不要把舊 app 改名回來，修正原因後在 release 資料夾再執行一次腳本；或原樣保留，交給擁有者判斷。

**從停止 WatchDog 到啟動後的檢查做完之前，任何一步失敗（包括 Ctrl-C）**，`trap` 接手：

1. 判斷在位的是**完整的舊 app**（從沒被改名）還是**完整的新 app**（和停止前做的內容記錄逐項相同；不再讀發布包，發布包所在的磁碟這時不見了也沒關係）。舊的已改名、新的還沒換上時，把舊的改名回來。
2. 放上相配的 LaunchAgent（新版用新寫的，舊版用原本的複本），啟動 WatchDog（已載入的 LaunchAgent 不再載入一次）並檢查 socket，說明在哪一步失敗、現在跑的是舊版還是新版。
3. 不碰 WGAL 保存的狀態，不拿備份蓋回去。
4. 照常比對 `scutil --nc list`。WatchDog 在執行時以 4 結束（1、2 優先）；沒有執行時以 1 結束並說明現在沒有任何監看。

新 WatchDog 的啟動一開始，新版就可能已經執行過、改過狀態：這之後的失敗（例如載入或等 socket 時按 Ctrl-C）**不會把舊 app 改名回來**，`trap` 再啟動一次在位的完整新 app，並說明新版可能已經執行過。

兩邊都湊不齊時不啟動，留下隱藏的 `.old`／`.new` 與 `upgrade/`，以 1 結束並說明。新版還沒啟動過時，之後再執行會以 65 拒絕（不刪舊 app）：先修正原因，自己把 `.WG Always Live.app.old` 改名回 `WG Always Live.app`（需要時用 sudo），再執行一次。

新版**可能已經執行過**（這次或之前某次已開始啟動新版）時，就算在位的是完整的舊 app，`trap` 也不啟動它，也不把舊 app 改名回來，以 1 結束，並說明：**不要把舊 app 放回去、不要啟動它**；先修正原因，把完整的新 app 放回原位（把 `/Applications/WG Always Live.app` 移開，用 `ditto` 把同一個發布包裡的 `WG Always Live.app` 複製過去，需要時用 sudo），再從那個發布包執行一次；或原樣保留，交給擁有者判斷。舊 app 已被改名成 `.WG Always Live.app.old` 而在位的不是完整新 app 時，再執行也會以 65 拒絕，並給同樣的說明。新版已經啟動、之後才沒有出現 socket 時不會自動退回舊版：新版可能已經改過狀態。

上面三種**不啟動**的情況（`started` 寫不進去、兩邊都湊不齊、新版可能已經執行過而在位的不是完整新 app），腳本在說明之前都先執行 `launchctl disable gui/<uid>/io.github.hsinken.wg-always-live.watchdog`（launchd 裡會一直留著的設定，只針對 WGAL 自己的 WatchDog），再以 `launchctl print-disabled gui/<uid>` 確認，免得下次登入時 launchd 依 LaunchAgent 的 `RunAtLoad` 自己啟動腳本剛拒絕啟動的那個 app 的 WatchDog；視窗的 LaunchAgent 不動（它只開視窗，不會啟動 WatchDog）。`!!!` 訊息會說已關閉登入時啟動，下一次通過檢查的執行在載入 WatchDog 之前會先 `launchctl enable`（launchd 不會載入被 disable 的服務；`trap` 復原時要啟動哪一個，也先 enable）；停止之前就以 65 拒絕的執行維持 disabled。關不掉時訊息直說關不掉、launchd 可能在下次登入時啟動它：請在登出或重新開機之前修正原因並再執行一次腳本，或原樣交給擁有者。`uninstall.sh` 移除 LaunchAgent 後也會 `launchctl enable` 這個 label，不留下 disable 的設定。

`install.sh` 的結束碼：0 成功；1 WatchDog 沒有以 socket 運作；2 VPN 設定有變化；4 升級在停止之後有一步失敗，WatchDog 已用在位的完整 app 啟動回來（只在 1、2 都不成立時）；64 用法錯誤；65 停止 WatchDog 之前的檢查或準備沒過：WatchDog 與在位的 app 維持原樣，但資料與紀錄資料夾、其中的 `uninstall.sh`、`upgrade/` 與隱藏的 `.WG Always Live.app.new` 可能已經換成新的。

這段流程的測試是 `tests/install-recovery.test.sh`。它把 `install.sh` 當函式庫載入（這時它什麼都不執行），路徑改到自己的暫存資料夾，以替身取代 `launchctl`、`pgrep`、`sleep`、`ditto`、`xattr`、`plutil`、`scutil`、`sudo` 與等 socket 的步驟，逐步注入失敗；`mv`、`rm`、`readlink`、`find` 與 `shasum` 是可以注入失敗的替身，沒有注入時執行系統本身的工具。它在任何 bash 裡都能跑（寫的時候用的是 WSL 的 bash 5）；2026-11-13 起也在一台 Mac 上以 macOS 內建的 bash 3.2 與 BSD 的工具執行。真正的 launchd、`ditto`、`scutil` 與 `sudo` 沒有在這裡測到；它不是實機的升級測試。替身只在測試裡，`install.sh` 沒有選用它們的選項或變數。

## 退回 4.0.0 以前的版本

用過 `wgal language <tag>` 之後（擁有者 2026-11-19 的決定），`~/Library/Application Support/WGAL/notify/notify.json` 存著 `wgal` 的語言，4.0.0 以前的 WatchDog 讀不懂這個檔：通知會停在關閉、也不接受通知設定的變更，直到檔案修好。

- 安裝舊版本之前，先在 4.0.0 以上執行 `wgal language --clear`（Telegram 訊息維持原來的語言），並確認 `wgal language` 顯示英文是預設。
- 舊版本已經因此在通知關閉的狀態下執行時：再安裝 4.0.0 以上，執行 `wgal language --clear`，然後再安裝舊版本。

升級前的備份（`backup/state-<UTC 時間>`）不能用來繞過：用過 `wgal language` 之後的升級所做的備份，裡面也有這個語言。這份 README 不提供在 macOS 放回備份的步驟。

## 簽章與 Gatekeeper

- **簽章**：
  - 這個 app 只有 ad hoc 簽章（`codesign --sign -`）。Apple 晶片只能執行有簽章的程式；建置腳本把三個程式（`wgal-watchdog`、`wgal-mcp`、`wgal`）放進 app 後，必須重新簽一次。
  - 沒有 Apple 開發者 ID 簽章，也沒有經過 Apple 公證（擁有者 2026-11-05 決定）。
- **安裝腳本做的事**：只對 `/Applications/WG Always Live.app` 移除下載隔離標記（`xattr -dr com.apple.quarantine`）。
- **不做的事**：不改系統的 Gatekeeper 設定，也不要求你關掉任何安全機制。
- **之後要改用開發者 ID 簽章與公證時**：改的是建置腳本，安裝方式不變；也可以改發已公證的 DMG。

## 停止與紀錄

- 暫時停止 WatchDog：`launchctl bootout gui/$(id -u)/io.github.hsinken.wg-always-live.watchdog`
- 再次啟動：重跑 `./install.sh`，或登出再登入。
- 紀錄：`tail -n 50 ~/Library/Logs/WGAL/watchdog.log`。事件紀錄由視窗的「紀錄」頁與 `wgal log` 顯示。

## 結束碼（紀錄中的 `exited with`）

| 碼 | 意思 |
|---|---|
| 1 | 沒有家目錄、路徑太長（socket 最多 103 bytes），或資料夾不是你的 0700 資料夾 |
| 2 | `state`、`notify` 或 `events` 不是你的 0700 資料夾 |
| 3 | 狀態檔無法開啟 |
| 4 | 監控迴圈無法啟動 |
| 5 | 以 root 執行（WatchDog 必須以使用者身分執行） |
| 11 | 監控迴圈停止回應；launchd 會重啟它 |

## 和 Windows 版的差異（尚未實作）

- 從睡眠喚醒、網路切換時，不會重新開始寬限期；仍由探測失敗本身判斷是否故障。
- WatchDog 只在使用者登入時執行，登出後與第一次登入前不監看（規格第 3.1 節）。

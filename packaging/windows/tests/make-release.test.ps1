#Requires -Version 5.1
# Tests of make-release.ps1's build record (FIX-06): -NoBuild packs the
# programs only when the record says they were built from this commit with
# this version and none changed since. Fake files in a temporary folder;
# nothing is built, packed or installed.
#
#   powershell.exe -NoProfile -File packaging\windows\tests\make-release.test.ps1

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Dot-sourced, the script only defines its functions: it builds nothing.
. (Join-Path $PSScriptRoot '..\make-release.ps1')

$failures = 0
function Check([string] $Name, [bool] $Ok) {
    if ($Ok) {
        Write-Host "PASS $Name"
    } else {
        Write-Host "FAIL $Name"
        $script:failures++
    }
}

$folder = Join-Path ([IO.Path]::GetTempPath()) ("wgal-make-release-test-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory $folder | Out-Null
$savedVariables = @{}
try {
    $files = @('wgal-watchdog.exe', 'wgal-desktop.exe', 'wgal-mcp.exe')
    foreach ($file in $files) {
        [IO.File]::WriteAllText((Join-Path $folder $file), "fake $file")
    }
    $record = Join-Path $folder 'wgal-build.txt'
    $commit = '0123456789abcdef0123456789abcdef01234567'
    $now = { param($c, $v) Get-BuildRecord $folder $c $v $files }

    Check 'no record is refused' ($null -ne (Test-BuildRecord $record (& $now $commit '4.2.1')))

    Write-BuildRecord $record (& $now $commit '4.2.1')
    Check 'the record holds one version' (@([IO.File]::ReadAllLines($record)) -contains 'version 4.2.1')
    Check 'the same commit, version and programs are accepted' ($null -eq (Test-BuildRecord $record (& $now $commit '4.2.1')))

    $why = Test-BuildRecord $record (& $now 'fedcba9876543210fedcba9876543210fedcba98' '4.2.1')
    Check 'another commit is refused' ($null -ne $why -and $why -like '*another commit*')

    $why = Test-BuildRecord $record (& $now $commit '4.2.2')
    Check 'another version is refused' ($null -ne $why -and $why -like '*version differs*')

    [IO.File]::WriteAllText((Join-Path $folder 'wgal-mcp.exe'), 'another build of the same version')
    $why = Test-BuildRecord $record (& $now $commit '4.2.1')
    Check 'a changed program is refused' ($null -ne $why -and $why -like '*changed*')

    Remove-Item (Join-Path $folder 'wgal-mcp.exe')
    Check 'a missing program is refused' ($null -ne (Test-BuildRecord $record (& $now $commit '4.2.1')))

    # 4.3.0: the build environment. Every variable that would add compiler
    # flags or move Cargo's output is refused, in any case, and only those.
    $a = [string][char]0xE4
    foreach ($name in @(Get-RefusedVariables)) {
        $savedVariables[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
        [Environment]::SetEnvironmentVariable($name, $null, 'Process')
    }
    Check 'no refused variable is left here' (@(Get-RefusedVariables).Count -eq 0)
    foreach ($name in @('RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CARGO_BUILD_RUSTFLAGS',
            'CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS', 'CARGO_TARGET_DIR', 'CARGO_BUILD_TARGET_DIR', 'CARGO_BUILD_TARGET')) {
        [Environment]::SetEnvironmentVariable($name, 'x', 'Process')
        $got = @(Get-RefusedVariables)
        Check "$name is refused" ($got.Count -eq 1 -and $got[0] -eq $name)
        [Environment]::SetEnvironmentVariable($name, $null, 'Process')
    }
    [Environment]::SetEnvironmentVariable('rustflags', 'x', 'Process')
    Check 'a name in lower case is refused' (@(Get-RefusedVariables).Count -eq 1)
    [Environment]::SetEnvironmentVariable('rustflags', $null, 'Process')
    foreach ($name in @('CARGO_TERM_COLOR', 'RUSTDOCFLAGS', 'CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER')) {
        $savedVariables[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
        [Environment]::SetEnvironmentVariable($name, 'x', 'Process')
    }
    Check 'other Cargo variables are not refused' (@(Get-RefusedVariables).Count -eq 0)

    # Cargo's configuration files: in the folders cargo runs in, every
    # folder above them, and Cargo's home.
    $cfg = Join-Path $folder "cfg dir $a"
    $crepo = Join-Path $cfg 'above\repo'
    $chome = Join-Path $cfg 'cargo home'
    $placed = @(
        (Join-Path $crepo 'apps\desktop\src-tauri\.cargo\config.toml'),
        (Join-Path $crepo 'apps\desktop\.cargo\config'),
        (Join-Path $crepo 'apps\.cargo\config.toml'),
        (Join-Path $crepo '.cargo\config'),
        (Join-Path $cfg 'above\.cargo\config.toml'),
        (Join-Path $chome 'config.toml'),
        (Join-Path $chome 'config')
    )
    foreach ($file in $placed) {
        New-Item -ItemType Directory -Force (Split-Path $file) | Out-Null
        [IO.File]::WriteAllText($file, "[net]`n")
    }
    $listed = @(Get-CargoConfigs $chome @($crepo, (Join-Path $crepo 'apps\desktop\src-tauri')))
    foreach ($file in $placed) {
        Check "the configuration $($file.Substring($cfg.Length + 1)) is looked at" ($listed -contains $file)
    }
    Check 'each configuration is looked at once' ($listed.Count -eq @($listed | Sort-Object -Unique).Count)
    Check 'harmless configurations are not refused' (@(Get-EnvironmentRefusals $crepo $chome).Count -eq 0)

    $one = Join-Path $folder 'one-config.toml'
    $refusal = { param([string[]] $Lines) [IO.File]::WriteAllText($one, ($Lines -join "`n") + "`n"); Get-ConfigRefusal $one }
    Check 'build.rustflags is refused' ((& $refusal '[build]', 'rustflags = ["-Cdebuginfo=2"]') -eq 'rustflags')
    Check 'target.<triple>.rustflags is refused' ((& $refusal '[target.x86_64-pc-windows-msvc]', 'rustflags = []') -eq 'rustflags')
    Check 'a dotted build.rustflags is refused' ((& $refusal 'build.rustflags = "-Cx"') -eq 'rustflags')
    Check 'an inline rustflags is refused' ((& $refusal 'build = { rustflags = [] }') -eq 'rustflags')
    Check 'rustflags in a comment is refused' ((& $refusal '# rustflags are set elsewhere') -eq 'rustflags')
    Check 'target-dir is refused' ((& $refusal '[build]', 'target-dir = "out"') -eq 'target-dir')
    Check 'a dotted target-dir is refused' ((& $refusal 'build.target-dir = "out"') -eq 'target-dir')
    Check 'build.target is refused' ((& $refusal '[build]', 'target = "x86_64-pc-windows-gnu"') -eq 'a key named target')
    Check 'a dotted build.target is refused' ((& $refusal 'build.target="x86_64-pc-windows-gnu"') -eq 'a key named target')
    Check 'an inline build.target is refused' ((& $refusal 'build = { target = "x" }') -eq 'a key named target')
    Check 'a quoted target key is refused' ((& $refusal '[build]', '"target" = "x"') -eq 'a key named target')
    $escape = 'an escape (\u, \U or \x) that could spell a key'
    Check 'a \u escape that could spell a key is refused' ((& $refusal '[build]', '"target\u002ddir" = "elsewhere"') -eq $escape)
    Check 'a \U escape is refused' ((& $refusal '"rust\U00000066lags" = []') -eq $escape)
    Check 'a \x escape is refused' ((& $refusal '"target\x2ddir" = "x"') -eq $escape)
    Check 'a backslash in a literal string before U is refused too' ((& $refusal "linker = 'C:\Users\cc'") -eq $escape)
    Check 'escaped backslashes are not an escape' ($null -eq (& $refusal 'linker = "C:\\Users\\cc"'))
    Check 'linker settings are not refused' ($null -eq (& $refusal '[target.x86_64-pc-windows-msvc]', 'linker = "lld-link"',
            "[target.'cfg(target_os = `"windows`")']", 'runner = "x"', '[net]', 'retry = 3'))
    $asFolder = Join-Path $folder 'a-folder.toml'
    New-Item -ItemType Directory $asFolder | Out-Null
    Check 'a configuration that is a folder is refused' ((Get-ConfigRefusal $asFolder) -eq 'it is not a readable file')
    [IO.File]::WriteAllText($one, "[net]`n")
    $lock = [IO.File]::Open($one, 'Open', 'Read', 'None')
    try {
        Check 'a configuration that cannot be read is refused' ((Get-ConfigRefusal $one) -eq 'it could not be read')
    } finally {
        $lock.Dispose()
    }

    $refusalAt = {
        param([string] $File, [string] $Content)
        [IO.File]::WriteAllText($File, $Content)
        $why = @(Get-EnvironmentRefusals $crepo $chome)
        [IO.File]::WriteAllText($File, "[net]`n")
        if ($why.Count -eq 1 -and -not $why[0].Contains($cfg)) { $why[0] } else { "unexpected: $($why.Count)" }
    }
    Check 'a refused configuration where the window is built' (
        (& $refusalAt $placed[0] 'target-dir = "x"') -eq 'the Cargo configuration in apps\desktop\src-tauri\.cargo\config.toml has target-dir')
    Check "a refused configuration in Cargo's home" (
        (& $refusalAt $placed[6] 'rustflags = []') -eq "the Cargo configuration in Cargo's home: config has rustflags")
    Check 'a refused configuration above the repository' (
        (& $refusalAt $placed[4] 'build.target = "x"') -eq 'the Cargo configuration in a folder above the repository: .cargo\config.toml has a key named target')
    [Environment]::SetEnvironmentVariable('CARGO_TARGET_DIR', 'x', 'Process')
    $why = @(Get-EnvironmentRefusals $crepo $chome)
    [Environment]::SetEnvironmentVariable('CARGO_TARGET_DIR', $null, 'Process')
    Check 'a refused variable is named' ($why.Count -eq 1 -and $why[0] -eq 'CARGO_TARGET_DIR is set; a release is built with its own compiler flags and output folder only')

    # Paths: absolute, and no part of them a link or a junction.
    $real = Join-Path $folder 'real one'
    New-Item -ItemType Directory (Join-Path $real 'sub') | Out-Null
    Check 'a relative path is taken from the given folder' ((Resolve-RealFolder $real '..') -eq $folder.TrimEnd('\'))
    Check 'a trailing separator is dropped' ((Resolve-RealFolder $folder "real one\") -eq $real)
    $threw = { param([scriptblock] $Block) try { & $Block | Out-Null; $false } catch { $true } }
    Check 'a missing folder is refused' (& $threw { Resolve-RealFolder $folder 'missing' })
    $junction = Join-Path $folder 'junction'
    New-Item -ItemType Junction -Path $junction -Target $real | Out-Null
    Check 'a junction is refused' (& $threw { Resolve-RealFolder $folder 'junction' })
    Check 'a folder reached through a junction is refused' (& $threw { Resolve-RealFolder $folder 'junction\sub' })
    Check 'a control character is found' (Test-HasControl "a$([char]0x1f)b")
    Check 'a newline is found' (Test-HasControl "a`nb")
    Check 'plain text has none' (-not (Test-HasControl "C:\a b\$a"))
    $safe = try { Stop-Safe 'mine' } catch { Get-SafeMessage $_ 'other' }
    $other = try { throw 'not mine' } catch { Get-SafeMessage $_ 'other' }
    Check 'a message of its own is told from another error' ($safe -eq 'mine' -and $other -eq 'other')
    $missing = try { Resolve-RealFolder $folder 'missing'; 'none' } catch { Get-SafeMessage $_ 'UNSAFE' }
    Check 'a missing folder: a message of its own' ($missing -eq 'it does not exist')
    $illegal = try { Resolve-RealFolder $folder 'bad<name>'; 'none' } catch { Get-SafeMessage $_ 'UNSAFE' }
    Check "another error while resolving a folder becomes a message of its own" ($illegal -eq 'it could not be read')

    # Spellings: each part in its long or its 8.3 short name.
    $originalNames = ${function:Get-FolderNames}
    try {
        Set-Item function:Get-FolderNames {
            param([string] $Folder)
            $leaf = Split-Path $Folder -Leaf
            switch ($leaf) {
                'long name one' { return @('long name one', 'LONGNA~1') }
                'LONGNA~1' { return @('long name one', 'LONGNA~1') }
                'long name two' { return @('long name two', 'LONGNA~2') }
                default { return @($leaf, $leaf.ToUpperInvariant()) }
            }
        }
        $spellings = @(Get-PathSpellings 'C:\a\long name one\b\long name two')
        Check 'every mix of long and short names is spelled' ($spellings.Count -eq 4 -and
            ($spellings -contains 'C:\a\long name one\b\long name two') -and ($spellings -contains 'C:\a\LONGNA~1\b\long name two') -and
            ($spellings -contains 'C:\a\long name one\b\LONGNA~2') -and ($spellings -contains 'C:\a\LONGNA~1\b\LONGNA~2'))
        Check 'a path given in short names gets its long names too' ((@(Get-PathSpellings 'C:\a\LONGNA~1') -join '|') -eq 'C:\a\LONGNA~1|C:\a\long name one')
        Check 'a short name that differs only in case is not doubled' (@(Get-PathSpellings 'C:\abc\def').Count -eq 1)
        Set-Item function:Get-FolderNames { param([string] $Folder) throw 'no short name' }
        Check 'a short name that cannot be read is refused' (& $threw { Get-PathSpellings 'C:\a' })
    } finally {
        Set-Item function:Get-FolderNames $originalNames
    }
    $longFolder = Join-Path $folder "a long folder name $a"
    New-Item -ItemType Directory $longFolder | Out-Null
    $names = @(Get-FolderNames $longFolder)
    if ($names[1] -ne $names[0]) {
        $short = Join-Path (Split-Path $longFolder) $names[1]
        Check 'a real folder: its short spelling is listed' (@(Get-PathSpellings $longFolder) -contains $short)
        Check 'a real folder given short: its long spelling is listed' (@(Get-PathSpellings $short) -contains $longFolder)
    } else {
        Write-Host 'SKIP a real folder: this volume makes no 8.3 short names'
    }

    # The scan before zipping: every file, UTF-8 and UTF-16LE, either case
    # of ASCII letters, either separator, every spelling; the message never
    # names the path.
    $sProfile = Join-Path $folder "scan profile $a"
    $sCargo = Join-Path $folder ("scan cargo " + [char]::ConvertFromUtf32(0x1D11E))
    $sRepo = Join-Path $folder 'scan repo'
    $sShort = 'C:\X\LONGFO~1\y'
    $forbidden = @(
        [pscustomobject]@{ Label = 'profile folder'; Spellings = @($sProfile) },
        [pscustomobject]@{ Label = 'Cargo home'; Spellings = @($sCargo) },
        [pscustomobject]@{ Label = 'repository'; Spellings = @($sRepo, 'C:\X\long folder\y', $sShort, 'C:\X\long folder\Y~1') }
    )
    $utf8 = New-Object Text.UTF8Encoding $false
    $rel = Join-Path $folder 'rel'
    $releaseLike = {
        if (Test-Path -LiteralPath $rel) { Remove-Item -LiteralPath $rel -Recurse -Force }
        New-Item -ItemType Directory (Join-Path $rel 'sub') | Out-Null
        [IO.File]::WriteAllBytes((Join-Path $rel 'wgal.exe'), [byte[]](0x4D, 0x5A, 0x00, 0x01))
        [IO.File]::WriteAllText((Join-Path $rel 'RELEASE.txt'), "plain`n")
        [IO.File]::WriteAllText((Join-Path $rel 'sub\file'), "nested`n")
    }
    $put = {
        param([string] $File, [string] $Text, [bool] $Utf16)
        $bytes = if ($Utf16) { [Text.Encoding]::Unicode.GetBytes("before$Text\x.rs after") } else { $utf8.GetBytes("before$Text\x.rs after") }
        [IO.File]::WriteAllBytes((Join-Path $rel $File), $bytes)
    }
    $scan = {
        $all = @(& { Find-BuildPaths $rel $forbidden } 6>&1)
        [pscustomobject]@{
            Hits = @($all | Where-Object { $_ -isnot [Management.Automation.InformationRecord] })[-1]
            Lines = @($all | Where-Object { $_ -is [Management.Automation.InformationRecord] } | ForEach-Object { $_.MessageData.ToString() })
        }
    }
    & $releaseLike
    $s = & $scan
    Check 'a clean release passes' ($s.Hits -eq 0)
    Check 'a clean release: every file counted' (($s.Lines -join '|') -eq 'Checked 3 files: no build paths')
    foreach ($case in @(@('profile folder', $sProfile), @('Cargo home', $sCargo), @('repository', $sRepo))) {
        foreach ($utf16 in @($false, $true)) {
            $shown = if ($utf16) { 'UTF-16LE' } else { 'UTF-8' }
            & $releaseLike
            & $put 'wgal.exe' $case[1] $utf16
            $s = & $scan
            Check "the $($case[0]) path in ${shown}: refused" ($s.Hits -eq 1)
            Check "the $($case[0]) path in ${shown}: the file, folder and encoding are named" ($s.Lines -contains "  wgal.exe: the path of the $($case[0]), $shown, 1")
            Check "the $($case[0]) path in ${shown}: the path is not printed" (-not (($s.Lines -join "`n").Contains($folder.TrimEnd('\'))))
        }
    }
    $asciiUpper = { param([string] $Text) [regex]::Replace($Text, '[a-z]', { param($m) $m.Value.ToUpperInvariant() }) }
    foreach ($variant in @(
            @('in upper case', (& $asciiUpper $sProfile)),
            @('in lower case', $sProfile.ToLowerInvariant()),
            @('with / for \', $sProfile.Replace('\', '/')),
            @('with both separators', ($sProfile.Substring(0, 3) + $sProfile.Substring(3).Replace('\', '/'))))) {
        foreach ($utf16 in @($false, $true)) {
            & $releaseLike
            & $put 'wgal.exe' $variant[1] $utf16
            Check "the profile path $($variant[0]), UTF-16LE ${utf16}: refused" ((& $scan).Hits -eq 1)
        }
    }
    & $releaseLike
    & $put 'wgal.exe' $sProfile.Replace($a, [string][char]0xC4) $false
    Check 'a non-ASCII letter in another case is not the same path' ((& $scan).Hits -eq 0)
    foreach ($spelling in @(@('all short', $sShort), @('long and short', 'C:\X\long folder\Y~1'), @('short in lower case', $sShort.ToLowerInvariant()))) {
        foreach ($utf16 in @($false, $true)) {
            & $releaseLike
            & $put 'wgal.exe' $spelling[1] $utf16
            Check "the repository spelled $($spelling[0]), UTF-16LE ${utf16}: refused" ((& $scan).Hits -eq 1)
        }
    }
    & $releaseLike
    [IO.File]::WriteAllText((Join-Path $rel 'RELEASE.txt'), "Commit 1`n$sRepo`n")
    $s = & $scan
    Check 'a path in RELEASE.txt is refused' ($s.Hits -eq 1 -and ($s.Lines -contains '  RELEASE.txt: the path of the repository, UTF-8, 1'))
    & $releaseLike
    [IO.File]::WriteAllText((Join-Path $rel 'sub\file'), "$sProfile$sProfile")
    Check 'a path in a nested file is counted' ((& $scan).Lines -contains '  sub\file: the path of the profile folder, UTF-8, 2')
    & $releaseLike
    New-Item -ItemType Junction -Path (Join-Path $rel 'link') -Target (Join-Path $rel 'sub') | Out-Null
    Check 'a junction in the release is refused' (& $threw { & $scan })
    [IO.Directory]::Delete((Join-Path $rel 'link'), $false)
    & $releaseLike
    $lock = [IO.File]::Open((Join-Path $rel 'wgal.exe'), 'Open', 'Read', 'None')
    try {
        $message = try { & $scan; '' } catch { $_.Exception.Message }
        Check 'a file that cannot be read is refused, named inside the release only' ($message -eq 'cannot read wgal.exe')
    } finally {
        $lock.Dispose()
    }
    $gone = try { Find-BuildPaths (Join-Path $folder 'no such release') $forbidden; 'none' } catch { Get-SafeMessage $_ 'UNSAFE' }
    Check 'a release folder that cannot be listed: a message of its own, with no path' ($gone -eq 'a folder of the release could not be listed')
    & $releaseLike
    Check 'no forbidden paths is refused' (& $threw { Find-BuildPaths $rel @() })
    Check 'a drive alone as a forbidden path is refused' (& $threw { Find-BuildPaths $rel @([pscustomobject]@{ Label = 'x'; Spellings = @('C:') }) })
} finally {
    # A junction is removed as a link first, so nothing is removed through it.
    $made = Join-Path $folder 'junction'
    if (Test-Path -LiteralPath $made) {
        [IO.Directory]::Delete($made, $false)
    }
    foreach ($name in @($savedVariables.Keys)) {
        [Environment]::SetEnvironmentVariable($name, $savedVariables[$name], 'Process')
    }
    Remove-Item -Recurse -Force $folder
}

if ($failures -gt 0) {
    Write-Host "$failures failed"
    exit 1
}
Write-Host 'all passed'

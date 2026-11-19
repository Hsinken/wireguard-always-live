#Requires -Version 5.1
<#
.SYNOPSIS
Builds the Windows release zip, dist\wgal-<version>-windows-x64.zip, where
the version is the one the whole set shares since 4.2.1 (the Cargo
workspace's; owner decision of 2026-11-19).
Run it only when the owner asks for a release.

.DESCRIPTION
From a clean checkout: builds wgal-watchdog, wgal-mcp, wgal and wgal-desktop in
release mode, puts them in dist\wgal-<version>-windows-x64\ with
install, uninstall, revert and setup (each as .ps1 and .cmd), README.md
and README.zh-TW.md, INSTALL.md and INSTALL.zh-TW.md (the guide an agent
can follow, in English and in Traditional Chinese) and RELEASE.txt (the
version, the commit and every file's SHA-256), and zips that folder. A
missing document stops it before the zip is made. It
needs no administrator rights and writes nothing outside target\ and dist\;
it installs nothing. The zip is not code-signed (owner decision of
2026-11-05).

The programs must not name the computer that built them (4.3.0):

- It builds only in the expected environment. It refuses, building
  nothing, when compiler flags or Cargo's output folder are set from
  outside: the variables RUSTFLAGS, CARGO_ENCODED_RUSTFLAGS,
  CARGO_BUILD_RUSTFLAGS, CARGO_TARGET_<triple>_RUSTFLAGS,
  CARGO_TARGET_DIR, CARGO_BUILD_TARGET_DIR or CARGO_BUILD_TARGET, or a
  Cargo configuration file (.cargo\config.toml or .cargo\config in a folder
  cargo runs in or any folder above it, or in Cargo's home) that mentions
  rustflags or target-dir or has a key named target.
- It takes Cargo's home (CARGO_HOME, relative to where the script was
  started, or %USERPROFILE%\.cargo), the repository and the user's profile
  folder as absolute paths, and refuses when any part of them is a link or
  a junction: Windows PowerShell 5.1 cannot follow one to its target
  reliably. It sets that CARGO_HOME and builds with only two flags, which
  write those two folders in the programs as /cargo and /wgal
  (CARGO_ENCODED_RUSTFLAGS, --remap-path-prefix). Both variables are put
  back as they were when it ends, built or not.
- The window is built with --locked too ('--' quoted, or Windows
  PowerShell drops it before pnpm), and the checkout must be unchanged
  after the builds (Cargo.lock included).
- Before zipping, every file of the release, RELEASE.txt included, is
  searched for the three folders' paths, as UTF-8 and as UTF-16LE, with
  ASCII letters in either case, either separator (\ or /), and every
  spelling that writes any part of a path as its 8.3 short name; a part
  whose short name cannot be read stops it. A hit, a link or junction, or
  any error zips nothing; the message names the file inside the release,
  which folder and which encoding, never the path itself.
- The same-name folder and zip in dist\ are deleted first; the release is
  assembled in a temporary folder and its zip moved into place only when
  everything passed, so a failed run leaves neither.

A build writes target\release\wgal-build.txt beside the programs: the
commit, the version and each program's SHA-256.

.PARAMETER NoBuild
Use the programs already in target\release instead of building them. Only
when wgal-build.txt says they were built from this very commit with this
version and none has changed since; otherwise nothing is packed (FIX-06:
a version mark alone does not tell one commit's build from another's).
They are checked for the three paths too.
#>
[CmdletBinding()]
param(
    [switch] $NoBuild
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$Root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$Programs = @('wgal-watchdog.exe', 'wgal-desktop.exe', 'wgal-mcp.exe', 'wgal.exe')
$Scripts = @('install.ps1', 'uninstall.ps1', 'revert.ps1', 'setup.ps1', 'install.cmd', 'uninstall.cmd', 'revert.cmd', 'setup.cmd')
# This folder's README in English and in Traditional Chinese, and the
# install guide every release carries (packaging\), in both.
$Readmes = @('README.md', 'README.zh-TW.md')
$Guides = @('INSTALL.md', 'INSTALL.zh-TW.md')

$RecordFile = 'target\release\wgal-build.txt'

# The build record's lines for the programs in $Folder as they are now.
function Get-BuildRecord([string] $Folder, [string] $Commit, [string] $Version, [string[]] $Files) {
    $lines = @("commit $Commit", "version $Version")
    foreach ($file in $Files) {
        $path = Join-Path $Folder $file
        $hash = if (Test-Path -LiteralPath $path -PathType Leaf) {
            (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLower()
        } else {
            'missing'
        }
        $lines += "$hash  $file"
    }
    return , $lines
}

function Write-BuildRecord([string] $Record, [string[]] $Lines) {
    [IO.File]::WriteAllLines($Record, $Lines, (New-Object Text.UTF8Encoding $false))
}

# Why the programs may not be packed as built from $Commit, or $null when
# the record says they were and none has changed.
function Test-BuildRecord([string] $Record, [string[]] $Expected) {
    if (-not (Test-Path -LiteralPath $Record -PathType Leaf)) {
        return "there is no build record ($RecordFile); build once without -NoBuild"
    }
    $recorded = @([IO.File]::ReadAllLines($Record))
    $commit = ($recorded | Where-Object { $_ -like 'commit *' } | Select-Object -First 1)
    if ($commit -ne $Expected[0]) {
        return "the programs were built from another commit ($commit), not $($Expected[0])"
    }
    if (($recorded -join "`n") -cne ($Expected -join "`n")) {
        return 'a program changed since it was built, or the version differs from the record'
    }
    return $null
}

function Invoke-Checked([string] $What, [scriptblock] $Command) {
    Write-Host "== $What"
    & $Command
    if ($LASTEXITCODE -ne 0) {
        throw "$What failed (exit $LASTEXITCODE)."
    }
}

# The variables that would add compiler flags or move Cargo's output, as
# themselves or as Cargo's own configuration keys (names compared as
# Windows does, in any case).
$RefusedVariables = @('RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CARGO_BUILD_RUSTFLAGS',
    'CARGO_TARGET_DIR', 'CARGO_BUILD_TARGET_DIR', 'CARGO_BUILD_TARGET')

function Get-RefusedVariables {
    $found = @()
    foreach ($item in @(Get-ChildItem Env:)) {
        $name = $item.Name.ToUpperInvariant()
        if ($RefusedVariables -contains $name -or $name -like 'CARGO_TARGET_*_RUSTFLAGS') {
            $found += $item.Name
        }
    }
    return $found
}

# Cargo's configuration files that apply to cargo run in any of $Folders:
# .cargo\config.toml and .cargo\config there and in every folder above it,
# and config.toml and config in Cargo's home. Each one that exists, once.
function Get-CargoConfigs([string] $CargoHome, [string[]] $Folders) {
    $candidates = @()
    foreach ($folder in $Folders) {
        $dir = $folder
        while ($dir) {
            $candidates += (Join-Path $dir '.cargo\config.toml'), (Join-Path $dir '.cargo\config')
            $dir = [IO.Path]::GetDirectoryName($dir)
        }
    }
    $candidates += (Join-Path $CargoHome 'config.toml'), (Join-Path $CargoHome 'config')
    $found = @()
    foreach ($file in $candidates) {
        if ((Test-Path -LiteralPath $file) -and -not ($found -contains $file)) {
            $found += $file
        }
    }
    return $found
}

# What a configuration file has that a release build refuses, whatever its
# TOML spelling: any mention of rustflags or target-dir (in a comment too),
# or a key named target (build.target moves the output to
# target\<triple>\). $null when it has none; a file that cannot be read is
# refused.
function Get-ConfigRefusal([string] $File) {
    if (-not (Test-Path -LiteralPath $File -PathType Leaf)) {
        return 'it is not a readable file'
    }
    try {
        $text = [IO.File]::ReadAllText($File)
    } catch {
        return 'it could not be read'
    }
    foreach ($key in @('rustflags', 'target-dir')) {
        if ($text.Contains($key)) {
            return $key
        }
    }
    if ($text -cmatch '(?m)(^|[^A-Za-z0-9_-])target["'']?[ \t]*=') {
        return 'a key named target'
    }
    return $null
}

# Where a configuration file is, without the account's folders.
function Get-ConfigPlace([string] $File, [string] $Repository, [string] $CargoHome) {
    $ignore = [StringComparison]::OrdinalIgnoreCase
    if ($File.StartsWith($Repository + '\', $ignore)) {
        return $File.Substring($Repository.Length + 1)
    }
    if ($File.StartsWith($CargoHome + '\', $ignore)) {
        return "Cargo's home: " + $File.Substring($CargoHome.Length + 1)
    }
    return 'a folder above the repository: .cargo\' + [IO.Path]::GetFileName($File)
}

# Why the build environment is refused, one reason per line; none when it
# is not. Cargo runs in the repository and, for the window, in
# apps\desktop\src-tauri.
function Get-EnvironmentRefusals([string] $Repository, [string] $CargoHome) {
    $why = @()
    foreach ($name in Get-RefusedVariables) {
        $why += "$name is set; a release is built with its own compiler flags and output folder only"
    }
    foreach ($file in Get-CargoConfigs $CargoHome @($Repository, (Join-Path $Repository 'apps\desktop\src-tauri'))) {
        $refusal = Get-ConfigRefusal $file
        if ($null -ne $refusal) {
            $why += "the Cargo configuration in $(Get-ConfigPlace $file $Repository $CargoHome) has $refusal"
        }
    }
    return $why
}

# The folder as an absolute path, a relative one taken from $From. Throws
# unless it exists and no part of it is a link or a junction.
function Resolve-RealFolder([string] $From, [string] $Path) {
    if (-not [IO.Path]::IsPathRooted($Path)) {
        $Path = Join-Path $From $Path
    }
    $full = [IO.Path]::GetFullPath($Path)
    $root = [IO.Path]::GetPathRoot($full)
    if ($full.Length -gt $root.Length) {
        $full = $full.TrimEnd('\')
    }
    if (-not (Test-Path -LiteralPath $full -PathType Container)) {
        throw 'it does not exist'
    }
    $part = $full
    while ($part -and $part.Length -gt $root.Length) {
        $item = Get-Item -LiteralPath $part -Force
        if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
            throw 'a part of it is a link or a junction'
        }
        $part = [IO.Path]::GetDirectoryName($part)
    }
    return $full
}

function Test-HasControl([string] $Text) {
    return $Text -match '[\x00-\x1f\x7f]'
}

# The folder's long name and its 8.3 short name (the same when it has
# none), whichever of the two its path was given in: read from its parent's
# listing, since a folder opened by its short name reports that name.
function Get-FolderNames([string] $Folder) {
    $parent = [IO.Path]::GetDirectoryName($Folder)
    $leaf = [IO.Path]::GetFileName($Folder)
    $fso = New-Object -ComObject Scripting.FileSystemObject
    foreach ($sub in $fso.GetFolder($parent).SubFolders) {
        if ($sub.Name -eq $leaf -or $sub.ShortName -eq $leaf) {
            if ([string]::IsNullOrEmpty($sub.Name) -or [string]::IsNullOrEmpty($sub.ShortName)) {
                break
            }
            return @($sub.Name, $sub.ShortName)
        }
    }
    throw 'no short name'
}

# Every spelling of the absolute path that writes each of its parts in its
# long or its 8.3 short name; throws when a part's names cannot be read.
function Get-PathSpellings([string] $Full) {
    $root = [IO.Path]::GetPathRoot($Full).TrimEnd('\')
    $spellings = @($root)
    $sofar = $root
    foreach ($part in @($Full.Substring($root.Length) -split '\\' | Where-Object { $_ })) {
        $sofar = "$sofar\$part"
        $names = @($part)
        foreach ($name in Get-FolderNames $sofar) {
            if (-not ($names -contains $name)) {
                $names += $name
            }
        }
        $spellings = @(foreach ($spelling in $spellings) { foreach ($name in $names) { "$spelling\$name" } })
    }
    return $spellings
}

# A regular expression for the path's bytes in UTF-8 or UTF-16LE, read as
# ISO-8859-1 (one character per byte): ASCII letters in either case, \ or /
# for either separator, every other character's bytes as they are.
function Get-PathPattern([string] $Path, [bool] $Utf16) {
    $utf8 = New-Object Text.UTF8Encoding $false
    $pattern = New-Object Text.StringBuilder
    $zero = if ($Utf16) { '\x00' } else { '' }
    $i = 0
    while ($i -lt $Path.Length) {
        $count = if ([char]::IsHighSurrogate($Path[$i]) -and $i + 1 -lt $Path.Length) { 2 } else { 1 }
        $text = $Path.Substring($i, $count)
        $i += $count
        $code = [int][char]$text[0]
        if ($count -eq 1 -and (($code -ge 0x41 -and $code -le 0x5A) -or ($code -ge 0x61 -and $code -le 0x7A))) {
            [void]$pattern.Append(('[\x{0:X2}\x{1:X2}]' -f ($code -band 0xDF), ($code -bor 0x20)) + $zero)
        } elseif ($count -eq 1 -and ($code -eq 0x5C -or $code -eq 0x2F)) {
            [void]$pattern.Append('[\x5C\x2F]' + $zero)
        } else {
            $bytes = if ($Utf16) { [Text.Encoding]::Unicode.GetBytes($text) } else { $utf8.GetBytes($text) }
            foreach ($byte in $bytes) {
                [void]$pattern.Append(('\x{0:X2}' -f $byte))
            }
        }
    }
    return $pattern.ToString()
}

# Every file under $Folder, as paths inside it, sorted; throws on a link or
# a junction.
function Get-ReleaseFiles([string] $Folder) {
    $files = @()
    $queue = New-Object 'System.Collections.Generic.Queue[string]'
    $queue.Enqueue($Folder)
    while ($queue.Count -gt 0) {
        $dir = $queue.Dequeue()
        foreach ($entry in [IO.Directory]::GetFileSystemEntries($dir)) {
            $attributes = [IO.File]::GetAttributes($entry)
            $relative = $entry.Substring($Folder.Length + 1)
            if ($attributes -band [IO.FileAttributes]::ReparsePoint) {
                throw "$relative is a link or a junction; a release holds only files and folders"
            }
            if ($attributes -band [IO.FileAttributes]::Directory) {
                $queue.Enqueue($entry)
            } else {
                $files += $relative
            }
        }
    }
    return @($files | Sort-Object)
}

# Searches every file under $Folder for the forbidden paths ($Forbidden:
# objects with a Label and the Spellings of one folder's path). Prints each
# hit as the file inside the folder, the label, the encoding and the count,
# never the path, and returns how many places it found; throws on any
# error, with a message that names no path but the file inside the folder.
function Find-BuildPaths([string] $Folder, [object[]] $Forbidden) {
    if (-not $Forbidden -or $Forbidden.Count -eq 0) {
        throw 'no forbidden paths were given'
    }
    $searches = @()
    foreach ($item in $Forbidden) {
        $spellings = @($item.Spellings)
        if ($spellings.Count -eq 0 -or @($spellings | Where-Object { $_.Length -lt 3 }).Count -gt 0) {
            throw 'a forbidden path is malformed'
        }
        foreach ($encoding in @('UTF-8', 'UTF-16LE')) {
            $alternatives = @($spellings | ForEach-Object { Get-PathPattern $_ ($encoding -eq 'UTF-16LE') })
            $searches += [pscustomobject]@{
                Label = $item.Label
                Encoding = $encoding
                Regex = New-Object Text.RegularExpressions.Regex ('(?:' + ($alternatives -join '|') + ')')
            }
        }
    }
    $latin1 = [Text.Encoding]::GetEncoding(28591)
    $folderPath = $Folder.TrimEnd('\')
    $hits = 0
    $files = @(Get-ReleaseFiles $folderPath)
    foreach ($relative in $files) {
        try {
            $text = $latin1.GetString([IO.File]::ReadAllBytes((Join-Path $folderPath $relative)))
        } catch {
            throw "cannot read $relative"
        }
        foreach ($search in $searches) {
            $count = $search.Regex.Matches($text).Count
            if ($count -gt 0) {
                $hits += $count
                Write-Host "  ${relative}: the path of the $($search.Label), $($search.Encoding), $count"
            }
        }
    }
    if ($hits -eq 0) {
        Write-Host "Checked $($files.Count) files: no build paths"
    }
    return $hits
}

function Invoke-Main {
$invoked = (Get-Location).ProviderPath
$refuse = { param($why) throw "make-release.ps1: $why" }
if (-not $env:USERPROFILE) { & $refuse 'USERPROFILE is not set' }
try {
    $repository = Resolve-RealFolder $invoked $Root
} catch {
    & $refuse "the repository's folder cannot be used: $($_.Exception.Message)"
}
try {
    $profileFolder = Resolve-RealFolder $invoked $env:USERPROFILE
} catch {
    & $refuse "the profile folder (USERPROFILE) cannot be used: $($_.Exception.Message)"
}
$cargoGiven = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
try {
    $cargoHome = Resolve-RealFolder $invoked $cargoGiven
} catch {
    & $refuse "Cargo's home folder (CARGO_HOME, or %USERPROFILE%\.cargo) cannot be used: $($_.Exception.Message)"
}
foreach ($path in @($repository, $profileFolder, $cargoHome)) {
    if (Test-HasControl $path) {
        & $refuse "a folder's path has a control character; it cannot be written out of the programs"
    }
}
try {
    $forbidden = @(
        [pscustomobject]@{ Label = 'profile folder'; Spellings = @(Get-PathSpellings $profileFolder) },
        [pscustomobject]@{ Label = 'Cargo home'; Spellings = @(Get-PathSpellings $cargoHome) },
        [pscustomobject]@{ Label = 'repository'; Spellings = @(Get-PathSpellings $repository) }
    )
} catch {
    & $refuse 'the 8.3 short names of the folders could not be read; nothing was built'
}

Push-Location $repository
$saved = @{}
$name = $null
$work = $null
$stage = $null
$zip = $null
$finished = $false
try {
    # git prints the top folder in the console's code page here, so ask
    # for the way up from here instead: none when this is the top.
    if ((git rev-parse --show-cdup | Out-String).Trim()) {
        & $refuse 'the script is not in the repository it runs in'
    }
    $dirty = git status --porcelain --untracked-files=no
    if ($dirty) {
        throw 'The checkout has uncommitted changes; a release is built from a commit.'
    }
    $commit = (git rev-parse HEAD).Trim()
    $workspace = Get-Content -Raw -Encoding UTF8 'Cargo.toml'
    if ($workspace -notmatch '(?ms)^\[workspace\.package\].*?^version\s*=\s*"([^"]+)"') {
        throw 'The version is not in Cargo.toml.'
    }
    $version = $Matches[1]

    $name = "wgal-$version-windows-x64"
    $dist = Join-Path $repository 'dist'
    $stage = Join-Path $dist $name
    $zip = Join-Path $dist "$name.zip"
    if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
    if (Test-Path -LiteralPath $zip) { Remove-Item -LiteralPath $zip -Force }

    $built = Join-Path $repository 'target\release'
    $record = Join-Path $repository $RecordFile
    if ($NoBuild) {
        $why = Test-BuildRecord $record (Get-BuildRecord $built $commit $version $Programs)
        if ($null -ne $why) {
            throw "-NoBuild: $why. Nothing was packed."
        }
    } else {
        $why = @(Get-EnvironmentRefusals $repository $cargoHome)
        if ($why.Count -gt 0) {
            & $refuse (($why -join "`n") + "`nNothing was built.")
        }
        foreach ($variable in @('CARGO_HOME', 'CARGO_ENCODED_RUSTFLAGS')) {
            $saved[$variable] = [Environment]::GetEnvironmentVariable($variable, 'Process')
        }
        $env:CARGO_HOME = $cargoHome
        $env:CARGO_ENCODED_RUSTFLAGS = "--remap-path-prefix=$cargoHome=/cargo" + [char]0x1f + "--remap-path-prefix=$repository=/wgal"
        Invoke-Checked 'wgal-watchdog' { cargo build --release --locked -p wgal-runtime --bin wgal-watchdog }
        Invoke-Checked 'wgal-mcp' { cargo build --release --locked -p wgal-mcp }
        Invoke-Checked 'wgal' { cargo build --release --locked -p wgal-cli }
        Invoke-Checked 'wgal-desktop' { pnpm --filter '@wgal/desktop' run tauri build --no-bundle '--' --locked }
        if ((git status --porcelain --untracked-files=no) -or ((git rev-parse HEAD).Trim() -ne $commit)) {
            & $refuse 'the build changed the checkout (Cargo.lock?); nothing was packed'
        }
        Write-BuildRecord $record (Get-BuildRecord $built $commit $version $Programs)
    }

    New-Item -ItemType Directory -Force $dist | Out-Null
    $work = Join-Path $dist (".$name." + [Guid]::NewGuid().ToString('N').Substring(0, 8))
    $temp = Join-Path $work $name
    New-Item -ItemType Directory $temp | Out-Null
    foreach ($file in $Programs) {
        Copy-Item -LiteralPath (Join-Path $built $file) $temp
    }
    foreach ($file in $Scripts + $Readmes) {
        Copy-Item -LiteralPath (Join-Path $repository "packaging\windows\$file") $temp
    }
    foreach ($file in $Guides) {
        Copy-Item -LiteralPath (Join-Path $repository "packaging\$file") $temp
    }

    $lines = @(
        "WG Always Live $version for Windows (x64)",
        "Version ${version}: the window, the watchdog, wgal-mcp and wgal",
        "Commit $commit",
        'Not code-signed. Install and remove with the scripts: see INSTALL.md (in Traditional Chinese: INSTALL.zh-TW.md).',
        '',
        'SHA-256'
    )
    foreach ($item in Get-ChildItem -LiteralPath $temp -File | Sort-Object Name) {
        $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $item.FullName).Hash.ToLower()
        $lines += "$hash  $($item.Name)"
    }
    [IO.File]::WriteAllLines((Join-Path $temp 'RELEASE.txt'), $lines, (New-Object Text.UTF8Encoding $false))

    try {
        $hits = Find-BuildPaths $temp $forbidden
    } catch {
        & $refuse "the release could not be checked ($($_.Exception.Message)); nothing was packed"
    }
    if ($hits -gt 0) {
        & $refuse "the release names the computer that built it ($hits places); nothing was packed"
    }

    Compress-Archive -Path $temp -DestinationPath (Join-Path $work "$name.zip")
    Move-Item -LiteralPath $temp -Destination $stage
    Move-Item -LiteralPath (Join-Path $work "$name.zip") -Destination $zip
    $finished = $true
    Remove-Item -LiteralPath $work -Recurse -Force
    $zipHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $zip).Hash.ToLower()
    Write-Host "Built $zip"
    Write-Host "SHA-256 $zipHash"
} finally {
    foreach ($variable in @($saved.Keys)) {
        [Environment]::SetEnvironmentVariable($variable, $saved[$variable], 'Process')
    }
    if (-not $finished) {
        foreach ($leftover in @($work, $stage, $zip)) {
            if ($leftover -and (Test-Path -LiteralPath $leftover)) {
                Remove-Item -LiteralPath $leftover -Recurse -Force
            }
        }
    }
    Pop-Location
}
}

# Dot-sourced (by the tests) it only defines the functions. Run, it prints
# only the message of what stopped it: PowerShell's own error display would
# add the script's place, which names the repository's folder.
if ($MyInvocation.InvocationName -ne '.') {
    try {
        Invoke-Main
    } catch {
        [Console]::Error.WriteLine($_.Exception.Message)
        exit 1
    }
}

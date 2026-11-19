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

- First it deletes this version's same-name release folder and zip in
  dist\, once the version is a plain name and neither dist\ nor they are a
  link or a junction (a folder is deleted without following one inside);
  whatever stops it later removes what it made, so a failed run leaves
  neither.
- It builds only in the expected environment. It refuses, building
  nothing, when compiler flags or Cargo's output folder are set from
  outside: the variables RUSTFLAGS, CARGO_ENCODED_RUSTFLAGS,
  CARGO_BUILD_RUSTFLAGS, CARGO_TARGET_<triple>_RUSTFLAGS,
  CARGO_TARGET_DIR, CARGO_BUILD_TARGET_DIR or CARGO_BUILD_TARGET, or a
  Cargo configuration file (.cargo\config.toml or .cargo\config in a folder
  cargo runs in or any folder above it, or config.toml or config in Cargo's
  home) that mentions rustflags or target-dir, has a key named target, or
  has an escape (\u, \U or \x) that could spell a key.
- It takes Cargo's home (CARGO_HOME, relative to where the script was
  started, or %USERPROFILE%\.cargo), the repository and the user's profile
  folder as absolute paths, and refuses when any part of them is a link or
  a junction: Windows PowerShell 5.1 cannot follow one to its target
  reliably. It sets that CARGO_HOME and builds with only two flags, which
  write those two folders in the programs as /cargo and /wgal
  (CARGO_ENCODED_RUSTFLAGS, --remap-path-prefix). Both variables are put
  back as they were when it ends, built or not.
- The window is built with --locked too ('--' quoted, or Windows
  PowerShell drops it before pnpm), and the checkout must be clean, without
  untracked files, before and after the builds (Cargo.lock included); every
  git call must succeed.
- Before zipping, every file of the release, RELEASE.txt included, is
  searched for the three folders' paths, as UTF-8 and as UTF-16LE, with
  ASCII letters in either case, either separator (\ or /), and every
  spelling that writes any part of a path as its 8.3 short name; a part
  whose short name cannot be read stops it. A hit, a link or junction, or
  any error zips nothing; the message names the file inside the release,
  which folder and which encoding, never the path itself.
- The release is assembled in a temporary folder and its zip moved into
  place last. Before that the zip is unpacked in the same folder and
  checked again: it must hold exactly the release's files, and its files
  are searched again. Every file this script writes is under target\ or
  dist\. When it stops, it prints only a message of its own, which names no
  folder; any other error is named by its kind only.

A build writes target\release\wgal-build.txt beside the programs: the
commit, the version, the rewrites' destinations (/cargo, /wgal) and each
program's SHA-256.

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

# A plain path, not resolved as a pattern: a folder name may hold [ or ].
$Root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
$Programs = @('wgal-watchdog.exe', 'wgal-desktop.exe', 'wgal-mcp.exe', 'wgal.exe')
$Scripts = @('install.ps1', 'uninstall.ps1', 'revert.ps1', 'setup.ps1', 'install.cmd', 'uninstall.cmd', 'revert.cmd', 'setup.cmd')
# This folder's README in English and in Traditional Chinese, and the
# install guide every release carries (packaging\), in both.
$Readmes = @('README.md', 'README.zh-TW.md')
$Guides = @('INSTALL.md', 'INSTALL.zh-TW.md')

$RecordFile = 'target\release\wgal-build.txt'

# The build record's lines for the programs in $Folder as they are now.
function Get-BuildRecord([string] $Folder, [string] $Commit, [string] $Version, [string[]] $Files) {
    $lines = @("commit $Commit", "version $Version", 'paths rewritten as /cargo and /wgal')
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
    # What a record says is not repeated: it may hold anything.
    $commits = @($recorded | Where-Object { $_ -like 'commit *' })
    if ($commits.Count -ne 1 -or $commits[0] -cnotmatch '^commit [0-9a-f]{40}$') {
        return "the build record ($RecordFile) is not one this script wrote; build once without -NoBuild"
    }
    if ($commits[0] -cne $Expected[0]) {
        return "the programs were built from another commit, not $($Expected[0].Substring(7))"
    }
    if (($recorded -join "`n") -cne ($Expected -join "`n")) {
        return 'a program changed since it was built, or the version differs from the record'
    }
    return $null
}

# Stops with a message of this script's own, which names no folder but
# files inside the release or the repository; Get-SafeMessage tells it
# from any other error, whose message may name a folder.
function Stop-Safe([string] $Message) {
    $exception = New-Object System.Exception $Message
    $exception.Data['wgal-make-release'] = $true
    throw $exception
}

function Get-SafeMessage([Management.Automation.ErrorRecord] $ErrorRecord, [string] $Otherwise) {
    if ($ErrorRecord.Exception.Data.Contains('wgal-make-release')) {
        return $ErrorRecord.Exception.Message
    }
    return $Otherwise
}

function Invoke-Checked([string] $What, [scriptblock] $Command) {
    Write-Host "== $What"
    & $Command
    if ($LASTEXITCODE -ne 0) {
        Stop-Safe "make-release.ps1: $What failed (exit $LASTEXITCODE)."
    }
}

# Runs git with the arguments and returns its output; stops when it fails.
# Its own messages, which may name folders, are not shown.
function Invoke-Git {
    $ErrorActionPreference = 'Continue'
    $output = & git @args 2>$null
    if ($LASTEXITCODE -ne 0) {
        Stop-Safe 'make-release.ps1: git could not read the checkout'
    }
    return $output
}

# What git says about the checkout, every change and untracked file included.
function Get-CheckoutState {
    return (@(Invoke-Git -c core.quotepath=true status --porcelain=v1 --untracked-files=all) -join "`n")
}

# Deletes a file or a folder this run made or replaces; a folder with
# everything in it, removing a link or junction inside as a link, never
# what it points to.
function Remove-Made([string] $Path) {
    if (Test-Path -LiteralPath $Path -PathType Container) {
        [IO.Directory]::Delete($Path, $true)
    } elseif (Test-Path -LiteralPath $Path) {
        [IO.File]::Delete($Path)
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
# an escape that could spell a key, or a key named target (build.target
# moves the output to target\<triple>\). $null when it has none; a file that
# cannot be read is refused.
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
    # \u, \U or \x after an even number of backslashes; in a literal string
    # too, which is refused rather than told apart.
    if ($text -cmatch '(^|[^\\])(\\\\)*\\[uUx]') {
        return 'an escape (\u, \U or \x) that could spell a key'
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
    try {
        if (-not [IO.Path]::IsPathRooted($Path)) {
            $Path = Join-Path $From $Path
        }
        $full = [IO.Path]::GetFullPath($Path)
        $root = [IO.Path]::GetPathRoot($full)
        if ($full.Length -gt $root.Length) {
            $full = $full.TrimEnd('\')
        }
        if (-not (Test-Path -LiteralPath $full -PathType Container)) {
            Stop-Safe 'it does not exist'
        }
        $part = $full
        while ($part -and $part.Length -gt $root.Length) {
            $item = Get-Item -LiteralPath $part -Force
            if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
                Stop-Safe 'a part of it is a link or a junction'
            }
            $part = [IO.Path]::GetDirectoryName($part)
        }
        return $full
    } catch {
        Stop-Safe (Get-SafeMessage $_ 'it could not be read')
    }
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
    Stop-Safe 'no short name'
}

# The text with the ASCII letters A to Z in lower case, every other
# character as it is.
function ConvertTo-AsciiLower([string] $Text) {
    return [regex]::Replace($Text, '[A-Z]', { param($m) $m.Value.ToLowerInvariant() })
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
            # Only ASCII letters' case is folded, as the search folds it:
            # spellings that differ in a non-ASCII letter's case are
            # different bytes, and both are kept.
            if (-not @($names | Where-Object { [string]::Equals((ConvertTo-AsciiLower $_), (ConvertTo-AsciiLower $name), [StringComparison]::Ordinal) })) {
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

# Zips the folder's files under the folder's own name, and unpacks a zip,
# with .NET's ZipFile, which takes paths as they are (Windows PowerShell's
# Compress-Archive reads [ and ] in a path as a pattern). Each entry is
# named with / between its parts, as zip files are meant to be: .NET's own
# CreateFromDirectory writes \ under Windows PowerShell.
function New-ReleaseZip([string] $Folder, [string] $Zip) {
    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $top = [IO.Path]::GetFileName($Folder.TrimEnd('\'))
    $archive = [IO.Compression.ZipFile]::Open($Zip, [IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach ($relative in @(Get-ReleaseFiles $Folder)) {
            [void][IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive, (Join-Path $Folder $relative),
                ($top + '/' + $relative.Replace('\', '/')), [IO.Compression.CompressionLevel]::Optimal)
        }
    } finally {
        $archive.Dispose()
    }
}

function Expand-ReleaseZip([string] $Zip, [string] $Destination) {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [IO.Compression.ZipFile]::ExtractToDirectory($Zip, $Destination)
}

# The zip's entry names, as the zip itself lists them, sorted ordinally.
function Get-ZipEntryNames([string] $Zip) {
    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [IO.Compression.ZipFile]::OpenRead($Zip)
    try {
        $names = [string[]]@($archive.Entries | ForEach-Object { $_.FullName })
    } finally {
        $archive.Dispose()
    }
    [Array]::Sort($names, [StringComparer]::Ordinal)
    return $names
}

# Every file under $Folder, as paths inside it, sorted; throws on a link or
# a junction.
function Get-ReleaseFiles([string] $Folder) {
    $files = @()
    $queue = New-Object 'System.Collections.Generic.Queue[string]'
    $queue.Enqueue($Folder)
    while ($queue.Count -gt 0) {
        $dir = $queue.Dequeue()
        try {
            $entries = [IO.Directory]::GetFileSystemEntries($dir)
        } catch {
            Stop-Safe 'a folder of the release could not be listed'
        }
        foreach ($entry in $entries) {
            $relative = $entry.Substring($Folder.Length + 1)
            try {
                $attributes = [IO.File]::GetAttributes($entry)
            } catch {
                Stop-Safe "$relative could not be inspected"
            }
            if ($attributes -band [IO.FileAttributes]::ReparsePoint) {
                Stop-Safe "$relative is a link or a junction; a release holds only files and folders"
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
        Stop-Safe 'no forbidden paths were given'
    }
    $searches = @()
    foreach ($item in $Forbidden) {
        $spellings = @($item.Spellings)
        if ($spellings.Count -eq 0 -or @($spellings | Where-Object { $_.Length -lt 3 }).Count -gt 0) {
            Stop-Safe 'a forbidden path is malformed'
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
            Stop-Safe "cannot read $relative"
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
# The repository first, with no link or junction anywhere in its path, so
# nothing is read or deleted through one.
try {
    $repository = Resolve-RealFolder $invoked $Root
} catch {
    Stop-Safe "make-release.ps1: the repository's folder cannot be used: $(Get-SafeMessage $_ 'it could not be read'); nothing was deleted or built"
}
# The release's name, from the version as written, before anything else.
try {
    $workspace = [IO.File]::ReadAllText((Join-Path $repository 'Cargo.toml'))
} catch {
    Stop-Safe 'make-release.ps1: Cargo.toml could not be read'
}
if ($workspace -notmatch '(?ms)^\[workspace\.package\].*?^version\s*=\s*"([^"]+)"') {
    Stop-Safe 'make-release.ps1: the version is not in Cargo.toml'
}
$version = $Matches[1]
if ($version -notmatch '^\d+\.\d+\.\d+$') {
    Stop-Safe 'make-release.ps1: the version is not a plain name; nothing was deleted or built'
}
$name = "wgal-$version-windows-x64"
$dist = Join-Path $repository 'dist'
$stage = Join-Path $dist $name
$zip = Join-Path $dist "$name.zip"
foreach ($place in @($dist, $stage, $zip)) {
    if ((Test-Path -LiteralPath $place) -and ((Get-Item -LiteralPath $place -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        Stop-Safe 'make-release.ps1: dist\ or the release in it is a link or a junction; nothing was deleted or built'
    }
}
if ((Test-Path -LiteralPath $dist) -and -not (Test-Path -LiteralPath $dist -PathType Container)) {
    Stop-Safe 'make-release.ps1: dist\ is not a folder; nothing was deleted or built'
}
Remove-Made $stage
Remove-Made $zip

$saved = @{}
$work = $null
$pushed = $false
$finished = $false
try {
    if (-not $env:USERPROFILE) { Stop-Safe 'make-release.ps1: USERPROFILE is not set' }
    try {
        $profileFolder = Resolve-RealFolder $invoked $env:USERPROFILE
    } catch {
        Stop-Safe "make-release.ps1: the profile folder (USERPROFILE) cannot be used: $(Get-SafeMessage $_ 'it could not be read')"
    }
    $cargoGiven = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
    try {
        $cargoHome = Resolve-RealFolder $invoked $cargoGiven
    } catch {
        Stop-Safe "make-release.ps1: Cargo's home folder (CARGO_HOME, or %USERPROFILE%\.cargo) cannot be used: $(Get-SafeMessage $_ 'it could not be read')"
    }
    foreach ($path in @($repository, $profileFolder, $cargoHome)) {
        if (Test-HasControl $path) {
            Stop-Safe "make-release.ps1: a folder's path has a control character; it cannot be written out of the programs"
        }
    }
    try {
        $forbidden = @(
            [pscustomobject]@{ Label = 'profile folder'; Spellings = @(Get-PathSpellings $profileFolder) },
            [pscustomobject]@{ Label = 'Cargo home'; Spellings = @(Get-PathSpellings $cargoHome) },
            [pscustomobject]@{ Label = 'repository'; Spellings = @(Get-PathSpellings $repository) }
        )
    } catch {
        Stop-Safe 'make-release.ps1: the 8.3 short names of the folders could not be read; nothing was built'
    }

    Push-Location -LiteralPath $repository
    $pushed = $true
    # git prints the top folder in the console's code page here, so ask for
    # the way up from here instead: none when this is the top.
    if ((@(Invoke-Git rev-parse --show-cdup) -join '').Trim()) {
        Stop-Safe 'make-release.ps1: the script is not in the repository it runs in'
    }
    if (Get-CheckoutState) {
        Stop-Safe 'make-release.ps1: the checkout has uncommitted changes or untracked files; a release is built from a clean checkout.'
    }
    $commit = (@(Invoke-Git rev-parse HEAD) -join '').Trim()

    $built = Join-Path $repository 'target\release'
    $record = Join-Path $repository $RecordFile
    if ($NoBuild) {
        $why = Test-BuildRecord $record (Get-BuildRecord $built $commit $version $Programs)
        if ($null -ne $why) {
            Stop-Safe "make-release.ps1: -NoBuild: $why. Nothing was packed."
        }
    } else {
        $why = @(Get-EnvironmentRefusals $repository $cargoHome)
        if ($why.Count -gt 0) {
            Stop-Safe ((@($why | ForEach-Object { "make-release.ps1: $_" }) + 'make-release.ps1: nothing was built') -join "`n")
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
        $after = Get-CheckoutState
        $now = (@(Invoke-Git rev-parse HEAD) -join '').Trim()
        if ($after -or $now -ne $commit) {
            Stop-Safe 'make-release.ps1: the build changed the checkout (Cargo.lock, or a new file?); nothing was packed'
        }
        Write-BuildRecord $record (Get-BuildRecord $built $commit $version $Programs)
    }

    # What goes in, each checked first so a missing one is named.
    $sources = @()
    foreach ($file in $Programs) { $sources += , @("target\release\$file", (Join-Path $built $file)) }
    foreach ($file in $Scripts + $Readmes) { $sources += , @("packaging\windows\$file", (Join-Path $repository "packaging\windows\$file")) }
    foreach ($file in $Guides) { $sources += , @("packaging\$file", (Join-Path $repository "packaging\$file")) }
    foreach ($source in $sources) {
        if (-not (Test-Path -LiteralPath $source[1] -PathType Leaf)) {
            Stop-Safe "make-release.ps1: $($source[0]) is missing; nothing was packed"
        }
    }
    New-Item -ItemType Directory -Force $dist | Out-Null
    $work = Join-Path $dist (".$name." + [Guid]::NewGuid().ToString('N').Substring(0, 8))
    $temp = Join-Path $work $name
    New-Item -ItemType Directory $temp | Out-Null
    foreach ($source in $sources) {
        Copy-Item -LiteralPath $source[1] $temp
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
        Stop-Safe "make-release.ps1: the release could not be checked ($(Get-SafeMessage $_ 'an error')); nothing was packed"
    }
    if ($hits -gt 0) {
        Stop-Safe "make-release.ps1: the release names the computer that built it ($hits places); nothing was packed"
    }

    New-ReleaseZip $temp (Join-Path $work "$name.zip")
    # The zip as it will be delivered. Its entries as the zip itself lists
    # them: exactly the release's files, each once, under the release's
    # folder and nothing beside it. Then the whole of what it unpacks to:
    # the same files, all of them searched again.
    $check = Join-Path $work 'check'
    try {
        $released = [string[]]@(Get-ReleaseFiles $temp | ForEach-Object { "$name\$_" })
        [Array]::Sort($released, [StringComparer]::Ordinal)
        $wanted = [string[]]@($released | ForEach-Object { $_.Replace('\', '/') })
        [Array]::Sort($wanted, [StringComparer]::Ordinal)
        $entries = @(Get-ZipEntryNames (Join-Path $work "$name.zip"))
        Expand-ReleaseZip (Join-Path $work "$name.zip") $check
        $unpacked = [string[]]@(Get-ReleaseFiles $check)
        [Array]::Sort($unpacked, [StringComparer]::Ordinal)
    } catch {
        Stop-Safe "make-release.ps1: the zip could not be checked ($(Get-SafeMessage $_ 'an error')); nothing was packed"
    }
    if (($entries -join "`n") -cne ($wanted -join "`n") -or ($unpacked -join "`n") -cne ($released -join "`n")) {
        Stop-Safe 'make-release.ps1: the zip holds other files than the release; nothing was packed'
    }
    try {
        $hits = Find-BuildPaths $check $forbidden
    } catch {
        Stop-Safe "make-release.ps1: the zip could not be checked ($(Get-SafeMessage $_ 'an error')); nothing was packed"
    }
    if ($hits -gt 0) {
        Stop-Safe "make-release.ps1: the zip names the computer that built it ($hits places); nothing was packed"
    }
    Move-Item -LiteralPath $temp -Destination $stage
    Move-Item -LiteralPath (Join-Path $work "$name.zip") -Destination $zip
    Remove-Made $work
    $zipHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $zip).Hash.ToLower()
    Write-Host "Built $zip"
    Write-Host "SHA-256 $zipHash"
    $finished = $true
} finally {
    foreach ($variable in @($saved.Keys)) {
        [Environment]::SetEnvironmentVariable($variable, $saved[$variable], 'Process')
    }
    if (-not $finished) {
        foreach ($leftover in @($work, $stage, $zip)) {
            if ($leftover) {
                try {
                    Remove-Made $leftover
                } catch {
                    Write-Host 'make-release.ps1: a leftover of this run in dist\ could not be removed'
                }
            }
        }
    }
    if ($pushed) {
        Pop-Location
    }
}
}

# Dot-sourced (by the tests) it only defines the functions. Run, it prints
# only the message of what stopped it: PowerShell's own error display would
# add the script's place, which names the repository's folder.
if ($MyInvocation.InvocationName -ne '.') {
    try {
        Invoke-Main
    } catch {
        [Console]::Error.WriteLine((Get-SafeMessage $_ "make-release.ps1: stopped by an unexpected error ($($_.Exception.GetType().Name)); nothing was packed"))
        exit 1
    }
}

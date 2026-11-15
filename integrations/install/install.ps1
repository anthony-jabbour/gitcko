# SPDX-License-Identifier: Apache-2.0
# Install gitcko into $env:GITCKO_INSTALL_DIR (default ~\.local\bin).
# Always checks SHA-256. Checks the minisign signature when minisign or rsign
# is on PATH; otherwise prints how to check it. Never elevates.
# The pinned public key is a TEST key until Anthony replaces it before the
# first real tag (integrations/install/minisign.pub must match these halves).

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false

$pinnedA = 'RWR5zJ23g86QtqzrU0bXyrUdiF'
$pinnedB = 'cdCin2EF6VfTUVfHL6b9XDQxmBjKKK'
$pinned = "$pinnedA$pinnedB"

function Die([string]$Message) {
    [Console]::Error.WriteLine($Message)
    exit 1
}

$scriptPath = $MyInvocation.MyCommand.Path
if ($scriptPath) {
    $pub = Join-Path (Split-Path -Parent $scriptPath) 'minisign.pub'
    if (Test-Path -LiteralPath $pub) {
        $fromFile = $null
        foreach ($line in Get-Content -LiteralPath $pub) {
            if ($line -and -not $line.StartsWith('untrusted')) {
                $fromFile = $line.Trim()
                break
            }
        }
        if ($fromFile -ne $pinned) {
            Die 'pinned public key does not match minisign.pub'
        }
    }
}

if (-not $env:USERPROFILE) { Die 'install failed' }

$base = 'https://gitcko.dev/releases'
if ($env:GITCKO_DOWNLOAD_URL) { $base = $env:GITCKO_DOWNLOAD_URL }
$installDir = Join-Path $env:USERPROFILE '.local\bin'
if ($env:GITCKO_INSTALL_DIR) { $installDir = $env:GITCKO_INSTALL_DIR }

while ($base.Length -gt 1 -and $base.EndsWith('/')) {
    $base = $base.Substring(0, $base.Length - 1)
}
if ($base.Length -gt 3 -and $base.EndsWith('\')) {
    $base = $base.TrimEnd('\')
}

function Decode-Spaces([string]$Text) {
    $decoded = $Text.Replace('%20', ' ')
    if ($decoded.Contains('%')) { Die 'download refused' }
    return $decoded
}

$mode = $null
$root = $null

function Classify {
    if ([string]::IsNullOrEmpty($base)) { Die 'download refused' }
    foreach ($ch in $base.ToCharArray()) {
        if ([int]$ch -lt 32 -or [int]$ch -eq 127) { Die 'download refused' }
    }
    if ($base -match '://[^/]*@') { Die 'download refused' }

    if ($base.StartsWith('/') -or $base -match '^[A-Za-z]:[\\/]') {
        $script:mode = 'dir'
        $script:root = Decode-Spaces $base
        return
    }
    if ($base.StartsWith('file://localhost/')) {
        $rest = $base.Substring('file://localhost/'.Length)
        $script:mode = 'dir'
        $script:root = Decode-Spaces (Normalize-FilePath $rest)
        return
    }
    if ($base.StartsWith('file:///')) {
        $rest = $base.Substring('file:///'.Length)
        $script:mode = 'dir'
        $script:root = Decode-Spaces (Normalize-FilePath $rest)
        return
    }
    if ($base.StartsWith('file://')) { Die 'download refused' }

    $https = $false
    $rest = $null
    if ($base.StartsWith('https://')) {
        $https = $true
        $rest = $base.Substring('https://'.Length)
    } elseif ($base.StartsWith('http://')) {
        $rest = $base.Substring('http://'.Length)
    } else {
        Die 'download refused'
    }

    $authority = $rest.Split('/')[0]
    if (-not $authority -or $authority.Contains(' ')) { Die 'download refused' }
    $hostName = $authority
    $port = $null
    if ($authority.StartsWith('[::1]')) {
        $hostName = '[::1]'
        if ($authority.Length -gt 5) {
            if (-not $authority.Substring(5).StartsWith(':')) { Die 'download refused' }
            $port = $authority.Substring(6)
        }
    } elseif ($authority.Contains(':')) {
        $parts = $authority.Split(':', 2)
        $hostName = $parts[0]
        $port = $parts[1]
    }
    if ($null -ne $port -and ($port -eq '' -or $port -notmatch '^[0-9]+$')) { Die 'download refused' }
    if (-not $hostName) { Die 'download refused' }
    if (-not $https -and $hostName -ne '127.0.0.1' -and $hostName -ne 'localhost' -and $hostName -ne '[::1]') {
        Die 'download refused'
    }
    if ($https) { $script:mode = 'https' } else { $script:mode = 'loopback' }
    $script:root = $base
}

function Normalize-FilePath([string]$Rest) {
    if ($Rest -match '^[A-Za-z]:') { return $Rest }
    return '/' + $Rest
}

Classify

$os = [System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform([System.Runtime.InteropServices.OSPlatform]::Windows)
$arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
if (-not $os -or "$arch" -ne 'X64') {
    Die 'this build does not ship a binary for this system'
}
$target = 'x86_64-pc-windows-msvc'
$archiveName = "gitcko-$target.zip"
$binName = "gitcko-$target.bin"

$work = Join-Path ([System.IO.Path]::GetTempPath()) ('gitcko-install-' + [guid]::NewGuid().ToString('n'))
New-Item -ItemType Directory -Path $work | Out-Null

try {
    function Fetch([string]$Name, [string]$Dest) {
        if ($Name -eq '.' -or $Name -eq '..' -or $Name -notmatch '^[A-Za-z0-9._-]+$') {
            Die 'download refused'
        }
        if ($mode -eq 'dir') {
            $src = Join-Path $root $Name
            $item = Get-Item -LiteralPath $src -Force
            if ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) {
                Die 'download refused'
            }
            Copy-Item -LiteralPath $src -Destination $Dest
            return
        }
        $url = "$root/$Name"
        $curl = Join-Path $env:SystemRoot 'System32\curl.exe'
        if (-not (Test-Path -LiteralPath $curl)) { $curl = 'curl.exe' }
        if ($mode -eq 'https') {
            & $curl -fsSL --proto '=https' --tlsv1.2 --max-redirs 0 -o $Dest $url 2>$null
        } elseif ($mode -eq 'loopback') {
            & $curl -fsSL --proto '=http' --max-redirs 0 -o $Dest $url 2>$null
        } else {
            Die 'download refused'
        }
        if ($LASTEXITCODE -ne 0) { Die 'download failed' }
    }

    $sums = Join-Path $work 'SHA256SUMS'
    $sig = Join-Path $work 'SHA256SUMS.minisig'
    Fetch 'SHA256SUMS' $sums
    Fetch 'SHA256SUMS.minisig' $sig
    $sigInfo = Get-Item -LiteralPath $sig -Force
    if ($sigInfo.Length -eq 0) { Die 'signature missing' }

    function Find-Verifier {
        foreach ($name in @('minisign', 'rsign')) {
            $cmd = Get-Command -Name $name -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
            if ($cmd) { return $cmd.Source }
        }
        return $null
    }

    $tool = Find-Verifier
    if ($tool) {
        $psi = New-Object System.Diagnostics.ProcessStartInfo
        $psi.FileName = 'cmd.exe'
        $psi.Arguments = "/c call `"$tool`" -Vm `"$sums`" -x `"$sig`" -P `"$pinned`""
        $psi.UseShellExecute = $false
        $psi.CreateNoWindow = $true
        $proc = [System.Diagnostics.Process]::Start($psi)
        $proc.WaitForExit()
        if ($proc.ExitCode -ne 0) { Die 'signature rejected' }
    } else {
        [Console]::Error.WriteLine('signature: not checked (minisign is not installed).')
        [Console]::Error.WriteLine("Check it with: minisign -Vm SHA256SUMS -P $pinned -x SHA256SUMS.minisig")
        [Console]::Error.WriteLine('Then run: gitcko doctor')
    }

    function Expect-Hash([string]$Name) {
        $found = $null
        foreach ($raw in Get-Content -LiteralPath $sums) {
            $line = $raw.TrimEnd("`r")
            if ($line -match '^([0-9A-Fa-f]{64})\s+\*?([A-Za-z0-9._-]+)$') {
                if ($Matches[2] -eq $Name) {
                    if ($found) { Die 'checksum mismatch' }
                    $found = $Matches[1].ToLower()
                }
            }
        }
        if (-not $found) { Die 'checksum mismatch' }
        return $found
    }

    function Hash-File([string]$Path) {
        return (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLower()
    }

    $archive = Join-Path $work $archiveName
    Fetch $archiveName $archive
    $want = Expect-Hash $archiveName
    $got = Hash-File $archive
    if ($got -ne $want) { Die 'checksum mismatch' }

    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $extract = Join-Path $work 'extract'
    New-Item -ItemType Directory -Path $extract | Out-Null
    $zip = [System.IO.Compression.ZipFile]::OpenRead($archive)
    try {
        if ($zip.Entries.Count -ne 1) { Die 'archive rejected' }
        $entry = $zip.Entries[0]
        if ($entry.FullName -ne 'gitcko.exe') { Die 'archive rejected' }
        $out = Join-Path $extract 'gitcko.exe'
        [System.IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $out, $false)
    } finally {
        $zip.Dispose()
    }
    $extracted = Get-Item -LiteralPath (Join-Path $extract 'gitcko.exe') -Force
    if ($extracted.Attributes -band [System.IO.FileAttributes]::ReparsePoint) { Die 'archive rejected' }
    if ($extracted.PSIsContainer) { Die 'archive rejected' }

    $wantBin = Expect-Hash $binName
    $gotBin = Hash-File $extracted.FullName
    if ($gotBin -ne $wantBin) { Die 'checksum mismatch' }

    if (-not $env:LOCALAPPDATA) { Die 'install failed' }
    $data = $env:LOCALAPPDATA
    if ($installDir.Contains("`n") -or $data.Contains("`n")) { Die 'install failed' }
    $release = Join-Path $data 'gitcko\release'
    New-Item -ItemType Directory -Force -Path $installDir | Out-Null
    New-Item -ItemType Directory -Force -Path $release | Out-Null
    Copy-Item -LiteralPath $sums -Destination (Join-Path $release 'SHA256SUMS') -Force
    Copy-Item -LiteralPath $sig -Destination (Join-Path $release 'SHA256SUMS.minisig') -Force

    $dest = Join-Path $installDir 'gitcko.exe'
    if (Test-Path -LiteralPath $dest) {
        $existing = Get-Item -LiteralPath $dest -Force
        if ($existing.Attributes -band [System.IO.FileAttributes]::ReparsePoint) {
            Die 'refusing to replace a symlink'
        }
    }
    $tmp = Join-Path $installDir ("gitcko.new." + $PID)
    Copy-Item -LiteralPath $extracted.FullName -Destination $tmp -Force
    try {
        Move-Item -LiteralPath $tmp -Destination $dest -Force
    } catch {
        if (Test-Path -LiteralPath $tmp) { Remove-Item -LiteralPath $tmp -Force }
        Die 'install failed'
    }

    $receipt = Join-Path $installDir 'gitcko-install.txt'
    $tmpReceipt = Join-Path $installDir ("gitcko-install.txt.new." + $PID)
    $utf8 = New-Object System.Text.UTF8Encoding $false
    [System.IO.File]::WriteAllText($tmpReceipt, "method=script`nrelease_dir=$release`n", $utf8)
    Move-Item -LiteralPath $tmpReceipt -Destination $receipt -Force
    [Console]::Out.WriteLine("installed: $dest")
} finally {
    if (Test-Path -LiteralPath $work) {
        Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
    }
}

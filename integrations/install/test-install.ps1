# Exercises install.ps1 against a local directory: a path with spaces, a bad
# checksum, a missing signature, and (when minisign is available) a real signature.
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false

$Here = Split-Path -Parent $MyInvocation.MyCommand.Path
$Install = Join-Path $Here 'install.ps1'
$HostExe = (Get-Process -Id $PID).Path
$OrigPath = $env:PATH
$Target = 'x86_64-pc-windows-msvc'
$ArchiveName = "gitcko-$Target.zip"
$BinName = "gitcko-$Target.bin"

function Fail([string]$Message) {
    [Console]::Error.WriteLine("FAIL $Message")
    exit 1
}

function New-Release([string]$Rel, [string]$Payload) {
    New-Item -ItemType Directory -Force -Path $Rel | Out-Null
    $stage = Join-Path $Rel 'stage'
    New-Item -ItemType Directory -Force -Path $stage | Out-Null
    $binary = Join-Path $stage 'gitcko.exe'
    Copy-Item -LiteralPath $Payload -Destination $binary -Force
    $archive = Join-Path $Rel $ArchiveName
    if (Test-Path -LiteralPath $archive) { Remove-Item -LiteralPath $archive -Force }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [System.IO.Compression.ZipFile]::Open($archive, 'Create')
    try {
        [void][System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $binary, 'gitcko.exe')
    } finally {
        $zip.Dispose()
    }
    $binHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $binary).Hash.ToLower()
    $arcHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $archive).Hash.ToLower()
    $lines = @(
        "$arcHash  $ArchiveName",
        "$binHash  $BinName"
    ) | Sort-Object
    $utf8 = New-Object System.Text.UTF8Encoding $false
    [System.IO.File]::WriteAllText((Join-Path $Rel 'SHA256SUMS'), (($lines -join "`n") + "`n"), $utf8)
}

function Flip-Last([string]$Path) {
    $bytes = [System.IO.File]::ReadAllBytes($Path)
    $bytes[$bytes.Length - 1] = $bytes[$bytes.Length - 1] -bxor 1
    [System.IO.File]::WriteAllBytes($Path, $bytes)
}

function Invoke-Install([string]$Url, [string]$Dest, [string]$Home, [string]$Data, [string]$PathValue) {
    $saved = @{
        URL = $env:GITCKO_DOWNLOAD_URL
        DEST = $env:GITCKO_INSTALL_DIR
        HOME = $env:USERPROFILE
        DATA = $env:LOCALAPPDATA
        PATH = $env:PATH
    }
    $env:GITCKO_DOWNLOAD_URL = $Url
    $env:GITCKO_INSTALL_DIR = $Dest
    $env:USERPROFILE = $Home
    $env:LOCALAPPDATA = $Data
    $env:PATH = $PathValue
    $id = [guid]::NewGuid().ToString('n')
    $outFile = Join-Path $env:TEMP "gitcko-install-out-$id.txt"
    $errFile = Join-Path $env:TEMP "gitcko-install-err-$id.txt"
    & $HostExe -NoProfile -File $Install > $outFile 2> $errFile
    $code = $LASTEXITCODE
    $out = ''
    $err = ''
    if (Test-Path -LiteralPath $outFile) { $out = [System.IO.File]::ReadAllText($outFile) }
    if (Test-Path -LiteralPath $errFile) { $err = [System.IO.File]::ReadAllText($errFile) }
    foreach ($name in @('GITCKO_DOWNLOAD_URL', 'GITCKO_INSTALL_DIR')) {
        Remove-Item "Env:$name" -ErrorAction SilentlyContinue
    }
    if ($null -ne $saved.URL) { $env:GITCKO_DOWNLOAD_URL = $saved.URL }
    if ($null -ne $saved.DEST) { $env:GITCKO_INSTALL_DIR = $saved.DEST }
    if ($null -ne $saved.HOME) { $env:USERPROFILE = $saved.HOME }
    if ($null -ne $saved.DATA) { $env:LOCALAPPDATA = $saved.DATA }
    if ($null -ne $saved.PATH) { $env:PATH = $saved.PATH }
    return @{ Code = $code; Out = $out; Err = $err }
}

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ('gitcko-install-test-' + [guid]::NewGuid().ToString('n'))
New-Item -ItemType Directory -Path $tmp | Out-Null
$clean = "$env:SystemRoot\System32"

try {
    $payload = Join-Path $tmp 'payload.bin'
    if ($env:GITCKO_TEST_BIN) {
        Copy-Item -LiteralPath $env:GITCKO_TEST_BIN -Destination $payload -Force
    } else {
        [System.IO.File]::WriteAllBytes($payload, [Text.Encoding]::ASCII.GetBytes("gitcko-test-ok"))
    }

    $good = Join-Path $tmp 'good'
    New-Release $good $payload
    [System.IO.File]::WriteAllText((Join-Path $good 'SHA256SUMS.minisig'), "not a signature, but present`n")

    $spaced = Join-Path $tmp 'dir with spaces\bin'
    $data = Join-Path $tmp 'data with spaces'
    $testHome = Join-Path $tmp 'home-ok'
    $result = Invoke-Install $good $spaced $testHome $data $clean
    if ($result.Code -ne 0) { Fail "good checksum should install: $($result.Err)" }
    if ($result.Out -notmatch '(?m)^installed:') { Fail 'missing installed line' }
    if ($result.Err -notmatch 'signature: not checked') { Fail 'should say the signature was not checked' }
    $installed = Join-Path $spaced 'gitcko.exe'
    if ((Get-FileHash $payload).Hash -ne (Get-FileHash $installed).Hash) { Fail 'installed bytes differ' }
    $receipt = Get-Content -LiteralPath (Join-Path $spaced 'gitcko-install.txt')
    if ($receipt[0] -ne 'method=script') { Fail 'receipt missing' }
    Write-Output 'ok install with spaces, checksum only'

    $bad = Join-Path $tmp 'bad'
    New-Item -ItemType Directory -Path $bad | Out-Null
    Copy-Item (Join-Path $good 'SHA256SUMS') $bad
    Copy-Item (Join-Path $good 'SHA256SUMS.minisig') $bad
    Copy-Item (Join-Path $good $ArchiveName) $bad
    Flip-Last (Join-Path $bad $ArchiveName)
    $kept = Join-Path $tmp 'keep with spaces\bin'
    New-Item -ItemType Directory -Force -Path $kept | Out-Null
    $old = Join-Path $kept 'gitcko.exe'
    [System.IO.File]::WriteAllText($old, "old-binary`n")
    $before = [System.IO.File]::ReadAllBytes($old)
    $result = Invoke-Install $bad $kept (Join-Path $tmp 'home-bad') (Join-Path $tmp 'data-bad') $clean
    if ($result.Code -eq 0) { Fail 'checksum mismatch must fail' }
    if ($result.Err -notmatch 'checksum mismatch') { Fail 'expected checksum mismatch' }
    $after = [System.IO.File]::ReadAllBytes($old)
    if ([Convert]::ToBase64String($before) -ne [Convert]::ToBase64String($after)) { Fail 'old binary was replaced' }
    Write-Output 'ok checksum mismatch leaves the old binary'

    $empty = Join-Path $tmp 'empty'
    New-Release $empty $payload
    [System.IO.File]::WriteAllText((Join-Path $empty 'SHA256SUMS.minisig'), '')
    $fresh = Join-Path $tmp 'empty-dest\bin'
    $result = Invoke-Install $empty $fresh (Join-Path $tmp 'home-empty') (Join-Path $tmp 'data-empty') $clean
    if ($result.Code -eq 0) { Fail 'empty signature must fail' }
    if ($result.Err -notmatch 'signature missing') { Fail 'expected signature missing' }
    if (Test-Path (Join-Path $fresh 'gitcko.exe')) { Fail 'installed despite empty signature' }
    Write-Output 'ok empty signature refused'

    $failing = Join-Path $tmp 'with-fail'
    New-Release $failing $payload
    [System.IO.File]::WriteAllText((Join-Path $failing 'SHA256SUMS.minisig'), "dummy`n")
    $failBin = Join-Path $tmp 'failbin'
    New-Item -ItemType Directory -Path $failBin | Out-Null
    [System.IO.File]::WriteAllText((Join-Path $failBin 'minisign.cmd'), "@echo off`r`nexit /b 1`r`n")
    $destFail = Join-Path $tmp 'fail-dest\bin'
    $result = Invoke-Install $failing $destFail (Join-Path $tmp 'home-fail') (Join-Path $tmp 'data-fail') "$failBin;$clean"
    if ($result.Code -eq 0) { Fail 'failing minisign must refuse' }
    if ($result.Err -notmatch 'signature rejected') { Fail "expected signature rejected: $($result.Err)" }
    if (Test-Path (Join-Path $destFail 'gitcko.exe')) { Fail 'installed despite failing minisign' }
    Write-Output 'ok failing minisign refuses'

    $okBin = Join-Path $tmp 'okbin'
    New-Item -ItemType Directory -Path $okBin | Out-Null
    [System.IO.File]::WriteAllText((Join-Path $okBin 'minisign.cmd'), "@echo off`r`nexit /b 0`r`n")
    $destOk = Join-Path $tmp 'fake-ok with spaces\bin'
    $result = Invoke-Install $failing $destOk (Join-Path $tmp 'home-fake-ok') (Join-Path $tmp 'data-fake-ok') "$okBin;$clean"
    if ($result.Code -ne 0) { Fail "minisign exit 0 should install: $($result.Err)" }
    if (-not (Test-Path (Join-Path $destOk 'gitcko.exe'))) { Fail 'fake verifier did not install' }
    Write-Output 'ok minisign exit 0 installs'

    $httpDest = Join-Path $tmp 'http-dest\bin'
    $result = Invoke-Install 'http://example.com/canary_update_secret' $httpDest (Join-Path $tmp 'home-http') (Join-Path $tmp 'data-http') $clean
    if ($result.Code -eq 0) { Fail 'public http must be refused' }
    if ($result.Err -notmatch 'download refused') { Fail 'expected download refused' }
    if ($result.Err -match 'canary_update_secret') { Fail 'error echoed the url' }
    if (Test-Path (Join-Path $httpDest 'gitcko.exe')) { Fail 'installed despite public http' }
    Write-Output 'ok public http refused'

    $fileDest = Join-Path $tmp 'file dest\bin'
    $fileUrl = 'file:///' + ($good -replace '\\', '/')
    $result = Invoke-Install $fileUrl $fileDest (Join-Path $tmp 'home-file') (Join-Path $tmp 'file-data') $clean
    if ($result.Code -ne 0) { Fail "file url should install: $($result.Err)" }
    if (-not (Test-Path (Join-Path $fileDest 'gitcko.exe'))) { Fail 'file url did not install' }
    Write-Output 'ok file url'

    if ($env:GITCKO_REAL_MINISIGN -eq '1') {
        $mini = Get-Command minisign -CommandType Application -ErrorAction SilentlyContinue
        if (-not $mini) { Fail 'GITCKO_REAL_MINISIGN set but minisign is not on PATH' }
        $real = Join-Path $tmp 'real'
        New-Release $real $payload
        & $mini.Source -S -W -s (Join-Path $Here 'testdata\minisign.secret') -m (Join-Path $real 'SHA256SUMS') -x (Join-Path $real 'SHA256SUMS.minisig') -t 'gitcko test' -q
        if ($LASTEXITCODE -ne 0) { Fail 'could not sign the test release' }
        $realDest = Join-Path $tmp 'real with spaces\bin'
        $result = Invoke-Install $real $realDest (Join-Path $tmp 'home-real') (Join-Path $tmp 'real-data') $OrigPath
        if ($result.Code -ne 0) { Fail "real signature should install: $($result.Err)" }
        if (-not (Test-Path (Join-Path $realDest 'gitcko.exe'))) { Fail 'real signature did not install' }

        $other = Join-Path $tmp 'other-key'
        New-Item -ItemType Directory -Path $other | Out-Null
        & $mini.Source -G -f -W -p (Join-Path $other 'pub') -s (Join-Path $other 'key')
        if ($LASTEXITCODE -ne 0) { Fail 'could not generate the other key' }
        $wrong = Join-Path $tmp 'wrong'
        New-Release $wrong $payload
        & $mini.Source -S -W -s (Join-Path $other 'key') -m (Join-Path $wrong 'SHA256SUMS') -x (Join-Path $wrong 'SHA256SUMS.minisig') -t 'other' -q
        if ($LASTEXITCODE -ne 0) { Fail 'could not sign with the other key' }
        $wrongDest = Join-Path $tmp 'wrong-dest\bin'
        New-Item -ItemType Directory -Force -Path $wrongDest | Out-Null
        $previous = Join-Path $wrongDest 'gitcko.exe'
        [System.IO.File]::WriteAllText($previous, "previous`n")
        $previousBytes = [System.IO.File]::ReadAllBytes($previous)
        $result = Invoke-Install $wrong $wrongDest (Join-Path $tmp 'home-wrong') (Join-Path $tmp 'data-wrong') $OrigPath
        if ($result.Code -eq 0) { Fail 'wrong key must be refused' }
        if ($result.Err -notmatch 'signature rejected') { Fail 'expected signature rejected for the other key' }
        $now = [System.IO.File]::ReadAllBytes($previous)
        if ([Convert]::ToBase64String($previousBytes) -ne [Convert]::ToBase64String($now)) { Fail 'wrong key replaced the binary' }

        $tamper = Join-Path $tmp 'tamper'
        New-Item -ItemType Directory -Path $tamper | Out-Null
        Copy-Item (Join-Path $real 'SHA256SUMS') $tamper
        Copy-Item (Join-Path $real 'SHA256SUMS.minisig') $tamper
        Copy-Item (Join-Path $real $ArchiveName) $tamper
        Flip-Last (Join-Path $tamper $ArchiveName)
        $tamperDest = Join-Path $tmp 'tamper-dest\bin'
        New-Item -ItemType Directory -Force -Path $tamperDest | Out-Null
        $keptBin = Join-Path $tamperDest 'gitcko.exe'
        [System.IO.File]::WriteAllText($keptBin, "previous`n")
        $keptBytes = [System.IO.File]::ReadAllBytes($keptBin)
        $result = Invoke-Install $tamper $tamperDest (Join-Path $tmp 'home-tamper') (Join-Path $tmp 'data-tamper') $OrigPath
        if ($result.Code -eq 0) { Fail 'tampered archive must be refused' }
        $keptNow = [System.IO.File]::ReadAllBytes($keptBin)
        if ([Convert]::ToBase64String($keptBytes) -ne [Convert]::ToBase64String($keptNow)) { Fail 'tampered archive replaced the binary' }
        Write-Output 'ok real minisign accepts the test key and refuses the other key'
    }

    Write-Output 'install.ps1 tests passed'
} finally {
    if (Test-Path -LiteralPath $tmp) {
        Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
}

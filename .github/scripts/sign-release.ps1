param(
    [Parameter(Mandatory = $true)]
    [string[]]$Path,
    [string]$ChecksumDirectory
)

$ErrorActionPreference = "Stop"

$certificateBase64 = $env:SEANPAK_CODESIGN_CERT_BASE64
$certificatePassword = $env:SEANPAK_CODESIGN_CERT_PASSWORD

if ([string]::IsNullOrWhiteSpace($certificateBase64) -and
    [string]::IsNullOrWhiteSpace($certificatePassword)) {
    Write-Host "::notice::Authenticode signing is not configured; release files will remain unsigned."
    exit 0
}
if ([string]::IsNullOrWhiteSpace($certificateBase64) -or
    [string]::IsNullOrWhiteSpace($certificatePassword)) {
    throw "Configure both SEANPAK_CODESIGN_CERT_BASE64 and SEANPAK_CODESIGN_CERT_PASSWORD."
}

$certificatePath = Join-Path $env:RUNNER_TEMP ("seanpak-codesign-{0}.pfx" -f $PID)
try {
    $certificateBytes = [Convert]::FromBase64String($certificateBase64)
    [System.IO.File]::WriteAllBytes($certificatePath, $certificateBytes)
    $certificateBytes = $null

    $sdkBin = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10\bin"
    $signTool = Get-ChildItem -LiteralPath $sdkBin -Filter "signtool.exe" -File -Recurse -ErrorAction Stop |
        Where-Object { $_.FullName -match "\\x64\\signtool\.exe$" } |
        Sort-Object { [version]$_.Directory.Parent.Name } -Descending |
        Select-Object -First 1
    if (-not $signTool) {
        throw "Windows SDK SignTool was not found on the runner."
    }

    foreach ($file in $Path) {
        if (-not (Test-Path -LiteralPath $file -PathType Leaf)) {
            throw "Release file not found: $file"
        }
        & $signTool.FullName sign /f $certificatePath /p $certificatePassword /fd SHA256 /tr https://timestamp.digicert.com /td SHA256 $file
        if ($LASTEXITCODE -ne 0) {
            throw "SignTool failed to sign $file (exit code $LASTEXITCODE)."
        }
        & $signTool.FullName verify /pa /all $file
        if ($LASTEXITCODE -ne 0) {
            throw "SignTool could not verify the signature on $file (exit code $LASTEXITCODE)."
        }
    }

    if (-not [string]::IsNullOrWhiteSpace($ChecksumDirectory)) {
        $assets = Get-ChildItem -LiteralPath $ChecksumDirectory -File |
            Where-Object { $_.Name -match "-Setup\.exe$" -or $_.Name -match "-Portable\.zip$" } |
            Sort-Object Name
        if ($assets.Count -ne 2) {
            throw "Expected exactly one Setup.exe and one Portable.zip in $ChecksumDirectory."
        }
        $lines = foreach ($asset in $assets) {
            $hash = (Get-FileHash -LiteralPath $asset.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
            "$hash  $($asset.Name)"
        }
        $checksumText = ($lines -join [Environment]::NewLine) + [Environment]::NewLine
        $encoding = [System.Text.UTF8Encoding]::new($false)
        [System.IO.File]::WriteAllText((Join-Path $ChecksumDirectory "SHA256SUMS.txt"), $checksumText, $encoding)
    }
}
finally {
    if (Test-Path -LiteralPath $certificatePath) {
        Remove-Item -LiteralPath $certificatePath -Force
    }
}

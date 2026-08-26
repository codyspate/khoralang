# Installs the newest Khora release candidate on Windows.
#
#     irm https://raw.githubusercontent.com/codyspate/khoralang/main/installrc.ps1 | iex
#
# `install.ps1` with `-Pre` already does this, and there is a second script
# because `iex` cannot pass an argument to what it is piped. The alternative
# was telling people to save the script to disk and run it with a flag, which
# is three lines and two concepts in the place where somebody is deciding
# whether this language is worth ten minutes.
#
# **A forwarder rather than a copy.** Two installers would be two files that
# have to stay true about checksums, layout and PATH, and the second one would
# rot first. This fetches `install.ps1` and runs it with `-Pre`, so there is
# one implementation of installing and this file only answers "which channel".
#
# `-Pre` means "candidates as well", not "candidates only". The day after a
# stable release, this installs that stable release -- which is the right
# answer for somebody who ran the candidate installer once and left it in a
# script.
[CmdletBinding()]
param(
    [string] $To = "",
    [switch] $NoModifyPath
)

$ErrorActionPreference = "Stop"
$Source = "https://raw.githubusercontent.com/codyspate/khoralang/main/install.ps1"

$script = Join-Path ([System.IO.Path]::GetTempPath()) ("khora-install-" + [guid]::NewGuid() + ".ps1")
try {
    try {
        Invoke-WebRequest -Uri $Source -OutFile $script -UseBasicParsing
    } catch {
        Write-Host ""
        Write-Host "install: could not fetch the installer from $Source" -ForegroundColor Red
        Write-Host "         $($_.Exception.Message)"
        Write-Host ""
        exit 1
    }

    # Splatted so that `-To` and `-NoModifyPath` reach it when this script is
    # run from disk, and are simply absent when it is piped into `iex`.
    $forward = @{ Pre = $true }
    if ($To) { $forward["To"] = $To }
    if ($NoModifyPath) { $forward["NoModifyPath"] = $true }
    & $script @forward
    exit $LASTEXITCODE
} finally {
    Remove-Item $script -Force -ErrorAction SilentlyContinue
}

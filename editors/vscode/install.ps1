# Builds the Khora extension from this checkout and installs it into VS Code.
#
# **Most people should not run this.** Download the `.vsix` from a `vscode-v*`
# release and `code --install-extension` it; this is for working *on* the
# extension, where the point is to install what is in the tree right now.
#
# Dropping the folder into ~/.vscode/extensions works only if VS Code happens to
# rescan the directory, and it never appears in its extensions index. Installing
# a real package registers it the same way a marketplace extension would, so it
# survives restarts and shows up in `code --list-extensions`.
#
# # Why this prefers `vsce`
#
# It used to build the zip by hand, on the grounds that a .vsix is an OPC
# package and PowerShell can write one without npm. It can — but the hand-built
# one copied `package.json`, the language configuration, the README and the
# syntaxes, and **not `src/extension.js` and not `node_modules`**. That
# package installs without complaint, contributes highlighting, and has no
# language server in it at all: no errors, no hover, no completion. Which is
# the failure a user reports as "the extension is broken".
#
# So `vsce` is used when Node is there, and the hand-rolled path is gone
# rather than kept as a fallback that produces something worse than nothing.

$ErrorActionPreference = "Stop"

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$pkg = Get-Content (Join-Path $here "package.json") -Raw | ConvertFrom-Json
$id = "$($pkg.publisher).$($pkg.name)"
$vsix = Join-Path $here "$id.vsix"

if (-not (Get-Command npm -ErrorAction SilentlyContinue)) {
    Write-Host "npm is not on PATH, and packaging an extension needs it." -ForegroundColor Yellow
    Write-Host ""
    Write-Host "  Install Node, or download the .vsix from a release instead:"
    Write-Host "  https://github.com/codyspate/khoralang/releases"
    Write-Host ""
    Write-Host "  then:  code --install-extension khora-vscode-<version>.vsix"
    exit 1
}

Push-Location $here
try {
    if (-not (Test-Path (Join-Path $here "node_modules"))) {
        npm ci
        if ($LASTEXITCODE -ne 0) { throw "npm ci failed" }
    }
    npm run package
    if ($LASTEXITCODE -ne 0) { throw "vsce package failed" }
} finally {
    Pop-Location
}

# The check the hand-rolled path needed and did not have. Windows PowerShell
# does not load the compression assembly on its own.
Add-Type -AssemblyName System.IO.Compression.FileSystem
$zip = [System.IO.Compression.ZipFile]::OpenRead($vsix)
try {
    $entries = $zip.Entries.FullName
    if (-not ($entries -contains "extension/src/extension.js")) {
        throw "$vsix has no extension.js -- it would install and do nothing"
    }
    if (-not ($entries | Where-Object { $_ -like "*vscode-languageclient*" })) {
        throw "$vsix has no language client -- it would install and do nothing"
    }
} finally {
    $zip.Dispose()
}

# A stale folder install shadows the packaged one, so clear it first.
$folder = Join-Path $env:USERPROFILE ".vscode\extensions\$id-$($pkg.version)"
if (Test-Path $folder) { Remove-Item $folder -Recurse -Force }

# `$ErrorActionPreference` says nothing about a native program's exit status,
# so this has to be read. It was not, and the script cheerfully reported
# success over "Please restart VS Code before reinstalling Khora" -- which is
# what `code` says when the running instance still holds the old version, and
# is a real failure: the tree you just built is not what is installed.
& code --install-extension $vsix --force
if ($LASTEXITCODE -ne 0) {
    Write-Host ""
    Write-Host "code --install-extension failed." -ForegroundColor Red
    Write-Host "If it asked you to restart VS Code, close every window -- including"
    Write-Host "the one you ran this from -- and run this again. The .vsix is built"
    Write-Host "and fine; it is the install that did not happen:"
    Write-Host "  $vsix"
    exit 1
}

Write-Host "`nInstalled $id $($pkg.version). Restart VS Code, then open a .kh file." -ForegroundColor Green

# Installs the Khora toolchain on Windows.
#
#     irm https://raw.githubusercontent.com/codyspate/khoralang/main/install.ps1 | iex
#
# For a release candidate, which needs the script on disk because `iex` takes
# no arguments:
#
#     irm https://raw.githubusercontent.com/codyspate/khoralang/main/install.ps1 -OutFile i.ps1
#     ./i.ps1 -Pre
#
# The counterpart to `install.sh`, which cannot run here: `curl | sh` needs a
# shell Windows does not have, and telling people to install one first is a
# worse first five minutes than a second script.
#
# Downloads the release for this machine, checks it against the published
# checksum, and unpacks it into %USERPROFILE%\.khora. Nothing is compiled,
# nothing needs administrator, and `Remove-Item -Recurse ~\.khora` undoes it.
#
# Run this once. After it, `khora` manages itself: `khora update` gets the next
# release, `khora toolchain install` gets a particular one, and
# `khora toolchain default` chooses between them.
[CmdletBinding()]
param(
    [string] $Version = "",
    [switch] $Pre,
    [string] $To = "",
    [switch] $NoModifyPath
)

$ErrorActionPreference = "Stop"
$Repo = "codyspate/khoralang"
$Home_ = if ($To) { $To } elseif ($env:KHORA_HOME) { $env:KHORA_HOME }
         else { Join-Path $env:USERPROFILE ".khora" }

function Fail($message) {
    Write-Host ""
    Write-Host "install: $message" -ForegroundColor Red
    Write-Host ""
    exit 1
}

$arch = switch ($env:PROCESSOR_ARCHITECTURE) {
    "AMD64" { "x86_64" }
    "ARM64" { "aarch64" }
    default { Fail "unsupported processor: $env:PROCESSOR_ARCHITECTURE" }
}
$triple = "$arch-pc-windows-msvc"

# Checked before downloading eighty megabytes: a toolchain that unpacks and
# then cannot link is a worse first five minutes than a warning. Not a
# refusal — somebody may be installing here to build elsewhere.
if (-not (Get-Command clang -ErrorAction SilentlyContinue)) {
    Write-Host ""
    Write-Host "  No C driver found on PATH."
    Write-Host ""
    Write-Host "  Khora compiles to a native object and needs one to link it against"
    Write-Host "  this platform's runtime - the same requirement rustc has here."
    Write-Host "    Visual Studio Build Tools, `"Desktop development with C++`""
    Write-Host "    or LLVM from https://releases.llvm.org"
    Write-Host ""
    Write-Host "  Installing anyway; ``khora build`` will say the same until one exists."
    Write-Host ""
}

# Two channels, and GitHub already had them: a candidate is published as a
# *pre-release*, which `/releases/latest` excludes. So a plain run never
# reaches one and `-Pre` is how somebody volunteers to test. `-Pre` means
# "include candidates" rather than "only candidates" — the narrower reading
# would install the candidate that preceded a stable release the day after it
# shipped.
#
# **A 404 from `/releases/latest` is an answer, not a failure**, and it is the
# answer whenever every release so far is a candidate. `Invoke-RestMethod`
# throws on one, and `$ErrorActionPreference = "Stop"` turns that into a page
# of PowerShell exception text ending in `WebCmdletWebResponseException` --
# which sends the reader to look at their network for something that is
# working exactly as designed. So it is caught, and what is said instead names
# the candidate that does exist and the command that installs it.
function Ask($url) {
    try {
        Invoke-RestMethod -Uri $url -Headers @{ "User-Agent" = "khora-install" }
    } catch {
        $null
    }
}

if ($Version) {
    $tag = "v" + $Version.TrimStart("v")
} elseif ($Pre) {
    $all = Ask "https://api.github.com/repos/$Repo/releases"
    $tag = ($all | Select-Object -First 1).tag_name
    if (-not $tag) { Fail "nothing has been released yet.`nSee https://github.com/$Repo/releases" }
} else {
    $tag = (Ask "https://api.github.com/repos/$Repo/releases/latest").tag_name
    if (-not $tag) {
        # Distinguish "nothing at all" from "candidates only", because the two
        # want different things from the reader.
        $any = Ask "https://api.github.com/repos/$Repo/releases"
        $newest = ($any | Select-Object -First 1).tag_name
        if ($newest) {
            Fail @"
no stable release yet. The newest is $newest, which is a candidate.

Install it with:

    irm https://raw.githubusercontent.com/$Repo/main/install.ps1 -OutFile i.ps1
    ./i.ps1 -Pre

iex cannot pass arguments, which is why this takes two lines.
"@
        }
        Fail "nothing has been released yet.`nSee https://github.com/$Repo/releases"
    }
}
$number = $tag.TrimStart("v")
$candidate = $number.Contains("-")

$name = "khora-$number-$triple"
$bundle = "$name.zip"
$base = "https://github.com/$Repo/releases/download/$tag"

if ($candidate) {
    Write-Host "Khora $number for $triple  (a release candidate)"
} else {
    Write-Host "Khora $number for $triple"
}

$work = Join-Path ([System.IO.Path]::GetTempPath()) ("khora-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $work | Out-Null
try {
    Write-Host "  downloading"
    try {
        Invoke-WebRequest "$base/$bundle" -OutFile (Join-Path $work $bundle)
        Invoke-WebRequest "$base/$bundle.sha256" -OutFile (Join-Path $work "$bundle.sha256")
    } catch {
        Fail "no build for $triple in $tag. See https://github.com/$Repo/releases/$tag"
    }

    Write-Host "  verifying"
    $expected = (Get-Content (Join-Path $work "$bundle.sha256") -Raw).Split()[0].ToLower()
    $actual = (Get-FileHash (Join-Path $work $bundle) -Algorithm SHA256).Hash.ToLower()
    if ($expected -ne $actual) {
        Fail "checksum mismatch:`n  expected $expected`n  got      $actual`nThe download is not what the release says it is. Do not use it."
    }

    Write-Host "  unpacking into $Home_"
    Expand-Archive -Path (Join-Path $work $bundle) -DestinationPath $work -Force
    # Replaced rather than merged: a leftover file from an older release is a
    # file the new compiler was never tested against.
    if (Test-Path $Home_) { Remove-Item -Recurse -Force $Home_ }
    New-Item -ItemType Directory -Path (Split-Path $Home_) -Force | Out-Null
    Move-Item (Join-Path $work $name) $Home_
} finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}

$bin = Join-Path $Home_ "bin"
$onPath = ($env:PATH -split ";") -contains $bin

if (-not $onPath -and -not $NoModifyPath) {
    # The user's own PATH, not the machine's: this needs no administrator and
    # affects nobody else on the box.
    $current = [Environment]::GetEnvironmentVariable("PATH", "User")
    if ($current -notlike "*$bin*") {
        [Environment]::SetEnvironmentVariable("PATH", "$bin;$current", "User")
        Write-Host "  added $bin to your PATH"
    }
}

Write-Host ""
Write-Host "Installed."
if (-not $onPath) {
    Write-Host ""
    Write-Host "  Open a new terminal, or for this one:"
    Write-Host "    `$env:PATH = `"$bin;`$env:PATH`""
}
Write-Host ""
Write-Host "  khora --help        what it can do"
Write-Host "  khora build .       compile the package in this directory"
Write-Host "  khora update        get the next release, when there is one"
if ($candidate) {
    Write-Host ""
    Write-Host "  This is a candidate. Please report what breaks:"
    Write-Host "    https://github.com/$Repo/issues"
}
Write-Host ""
Write-Host "  Uninstall with: Remove-Item -Recurse -Force $Home_"

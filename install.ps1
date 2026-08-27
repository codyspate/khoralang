# Installs the Khora toolchain on Windows.
#
#     irm https://raw.githubusercontent.com/codyspate/khoralang/main/install.ps1 | iex
#
# For a release candidate, `installrc.ps1` is this with `-Pre`:
#
#     irm https://raw.githubusercontent.com/codyspate/khoralang/main/installrc.ps1 | iex
#
# It exists because `iex` cannot pass an argument to what it is piped, and the
# alternative -- save to disk, run with a flag -- is three lines in the place
# where somebody is deciding whether this is worth ten minutes.
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
# *pre-release*. So a plain run never reaches one and `-Pre` is how somebody
# volunteers to test. `-Pre` means "include candidates" rather than "only
# candidates" — the narrower reading would install the candidate that preceded
# a stable release the day after it shipped.
#
# **A failed request is an answer, not a failure**, and an empty list is the
# answer whenever every release so far is a candidate. `Invoke-RestMethod`
# throws on a 404, and `$ErrorActionPreference = "Stop"` turns that into a page
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

# **`/releases/latest` is not asked any more, and the reason is a bug this
# had.** It means "the newest release in this repository that is not a draft or
# a pre-release" -- the whole repository. The VS Code extension is released
# from here too, on `vscode-v*` tags, and it is not a pre-release, because
# there is nothing provisional about it. So `/releases/latest` began returning
# the editor extension, `TrimStart("v")` turned `vscode-v0.3.0` into
# `scode-v0.3.0`, and a plain run went looking for
# `khora-scode-v0.3.0-<triple>.zip`. `-Pre` broke the same way, because
# `/releases` is newest first and the extension was newest.
#
# A tag names a toolchain when it is `v` and then a digit. `vscode-v0.3.0` also
# starts with `v`; the digit is what tells them apart.
function Toolchains($repo) {
    $all = Ask "https://api.github.com/repos/$repo/releases"
    if (-not $all) { return @() }
    @($all | Where-Object { $_.tag_name -match '^v[0-9]' -and -not $_.draft })
}

if ($Version) {
    $tag = "v" + $Version.TrimStart("v")
} elseif ($Pre) {
    $tag = (Toolchains $Repo | Select-Object -First 1).tag_name
    if (-not $tag) { Fail "nothing has been released yet.`nSee https://github.com/$Repo/releases" }
} else {
    $toolchains = Toolchains $Repo
    $tag = ($toolchains | Where-Object { -not $_.prerelease } | Select-Object -First 1).tag_name
    if (-not $tag) {
        # Distinguish "nothing at all" from "candidates only", because the two
        # want different things from the reader.
        $newest = ($toolchains | Select-Object -First 1).tag_name
        if ($newest) {
            Fail @"
no stable release yet. The newest is $newest, which is a candidate.

Install it with:

    irm https://raw.githubusercontent.com/$Repo/main/installrc.ps1 | iex

Or from here, with -Pre.
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
        # `/releases/tag/<tag>`, not `/releases/<tag>`, which 404s. And the
        # second sentence, because the likeliest reason to be here is not a
        # missing platform: it is a release whose artifacts are still being
        # built, which is the window the draft-first flow exists to close.
        Fail @"
no build for $triple in $tag yet.

If that release was just created, its artifacts are still building -- try again
in a few minutes. Otherwise this platform was not published for it.

See https://github.com/$Repo/releases/tag/$tag for what is there.
"@
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

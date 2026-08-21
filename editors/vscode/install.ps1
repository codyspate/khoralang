# Packages the Khora extension as a .vsix and installs it into VS Code.
#
# Dropping the folder into ~/.vscode/extensions works only if VS Code happens to
# rescan the directory, and it never appears in its extensions index. Installing
# a real package registers it the same way a marketplace extension would, so it
# survives restarts and shows up in `code --list-extensions`.
#
# Needs no npm: a .vsix is a zip with an OPC manifest, which PowerShell can build.

$ErrorActionPreference = "Stop"

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$pkg = Get-Content (Join-Path $here "package.json") -Raw | ConvertFrom-Json
$id = "$($pkg.publisher).$($pkg.name)"
$staging = Join-Path ([System.IO.Path]::GetTempPath()) "khora-vsix"
$vsix = Join-Path $here "$id-$($pkg.version).vsix"

if (Test-Path $staging) { Remove-Item $staging -Recurse -Force }
New-Item -ItemType Directory -Path (Join-Path $staging "extension") | Out-Null

foreach ($item in @("package.json", "language-configuration.json", "README.md", "syntaxes")) {
    $src = Join-Path $here $item
    if (Test-Path $src) {
        Copy-Item $src -Destination (Join-Path $staging "extension") -Recurse -Force
    }
}

$contentTypes = @'
<?xml version="1.0" encoding="utf-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="json" ContentType="application/json" />
  <Default Extension="md" ContentType="text/markdown" />
  <Default Extension="vsixmanifest" ContentType="text/xml" />
</Types>
'@
# [Content_Types].xml must be written with .NET: PowerShell treats the square
# brackets in the filename as a wildcard.
$utf8 = New-Object System.Text.UTF8Encoding($false)
[System.IO.File]::WriteAllText((Join-Path $staging "[Content_Types].xml"), $contentTypes, $utf8)

$manifest = @"
<?xml version="1.0" encoding="utf-8"?>
<PackageManifest Version="2.0.0" xmlns="http://schemas.microsoft.com/developer/vsx-schema/2011">
  <Metadata>
    <Identity Language="en-US" Id="$($pkg.name)" Version="$($pkg.version)" Publisher="$($pkg.publisher)" />
    <DisplayName>$($pkg.displayName)</DisplayName>
    <Description xml:space="preserve">$($pkg.description)</Description>
    <Categories>Programming Languages</Categories>
  </Metadata>
  <Installation>
    <InstallationTarget Id="Microsoft.VisualStudio.Code" />
  </Installation>
  <Dependencies />
  <Assets>
    <Asset Type="Microsoft.VisualStudio.Code.Manifest" Path="extension/package.json" Addressable="true" />
  </Assets>
</PackageManifest>
"@
[System.IO.File]::WriteAllText((Join-Path $staging "extension.vsixmanifest"), $manifest, $utf8)

if (Test-Path $vsix) { Remove-Item $vsix -Force }
Compress-Archive -Path (Join-Path $staging "*") -DestinationPath "$vsix.zip" -Force
Move-Item "$vsix.zip" $vsix -Force

# A stale folder install shadows the packaged one, so clear it first.
$folder = Join-Path $env:USERPROFILE ".vscode\extensions\$id-$($pkg.version)"
if (Test-Path $folder) { Remove-Item $folder -Recurse -Force }

& code --install-extension $vsix --force
Write-Host "`nInstalled $id. Restart VS Code, then open a .kh file." -ForegroundColor Green

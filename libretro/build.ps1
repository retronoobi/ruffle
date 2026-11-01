#Requires -Version 7.0
[CmdletBinding()]
param(
    [ValidateSet('Build','Check','Test','Package')][string]$Task='Build',
    [switch]$Offline,
    [switch]$GraphicsTests,
    [string]$SdlPath,
    [string]$OutputDirectory
)
$ErrorActionPreference='Stop'
$repo=Split-Path -Parent $PSScriptRoot
if(-not $IsWindows){throw 'The supported release target is Windows x64 GNU.'}
foreach($tool in @('cargo','rustc','gcc','java')){
    if(-not (Get-Command $tool -ErrorAction SilentlyContinue)){throw "Missing $tool. See libretro/README.md for build requirements."}
}
$hostInfo=& rustc -vV
if($LASTEXITCODE -ne 0 -or $hostInfo -notcontains 'host: x86_64-pc-windows-gnu'){
    throw 'Select the x86_64-pc-windows-gnu Rust toolchain before building.'
}
if(-not $OutputDirectory){$OutputDirectory=Join-Path $PSScriptRoot 'dist'}
$targetDirectory=if($env:CARGO_TARGET_DIR){[IO.Path]::GetFullPath($env:CARGO_TARGET_DIR)}else{Join-Path $repo 'target'}
$dll=Join-Path $targetDirectory 'release/ruffle_libretro.dll'
function Invoke-Cargo([string[]]$Arguments){
    & cargo @Arguments
    if($LASTEXITCODE -ne 0){throw "cargo $($Arguments -join ' ') failed ($LASTEXITCODE)"}
}
$oldSdl=$env:RUFFLE_TEST_SDL2
Push-Location $repo
try {
    $arguments=@('-p','ruffle_libretro','--locked')
    if($Offline){$arguments+='--offline'}
    if($Task -eq 'Check'){
        Invoke-Cargo (@('check')+$arguments)
        return
    }
    if($Task -eq 'Test'){
        $testArguments=@('test')+$arguments+@('--release','--lib','--','--test-threads=1')
        if($GraphicsTests){
            if(-not $SdlPath){throw '-GraphicsTests requires -SdlPath pointing to a local SDL2.dll.'}
            $env:RUFFLE_TEST_SDL2=(Resolve-Path -LiteralPath $SdlPath).Path
            $testArguments+='--include-ignored'
        }
        Invoke-Cargo $testArguments
        return
    }
    Invoke-Cargo (@('build')+$arguments+@('--release'))
    New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
    $manifest=Get-Content -LiteralPath (Join-Path $PSScriptRoot 'Cargo.toml') -Raw
    $match=[regex]::Match($manifest,'(?m)^version\s*=\s*"([^"]+)"')
    if(-not $match.Success){throw 'Cannot read libretro version'}
    $version=$match.Groups[1].Value
    Copy-Item -LiteralPath $dll -Destination (Join-Path $OutputDirectory 'ruffle_libretro.dll')
    $info=Get-Content -LiteralPath (Join-Path $PSScriptRoot 'ruffle_libretro.info') -Raw
    $info=[regex]::Replace($info,'(?m)^display_version\s*=.*$',('display_version = "'+$version+'"'))
    [IO.File]::WriteAllText((Join-Path $OutputDirectory 'ruffle_libretro.info'),$info)
    if($Task -eq 'Build'){return}
    if(-not (Get-Command python -ErrorAction SilentlyContinue)){throw 'Packaging requires Python 3.11 or later.'}
    & python (Join-Path $PSScriptRoot 'scripts/check_core.py') --dll (Join-Path $OutputDirectory 'ruffle_libretro.dll') --info (Join-Path $OutputDirectory 'ruffle_libretro.info') --version $version
    if($LASTEXITCODE -ne 0){throw 'Core ABI/package validation failed'}
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'README.md') -Destination (Join-Path $OutputDirectory 'README.md')
    Copy-Item -LiteralPath (Join-Path $repo 'LICENSE.md') -Destination (Join-Path $OutputDirectory 'LICENSE.md')
    $revision=& git rev-parse HEAD
    if($LASTEXITCODE -ne 0){throw 'Cannot determine source commit'}
    $compiler=& rustc --version
    $metadata=[ordered]@{version=$version;revision=$revision;rustc=$compiler;target='x86_64-pc-windows-gnu';renderer='OpenGL';network=$false}
    [IO.File]::WriteAllText((Join-Path $OutputDirectory 'BUILD.json'),($metadata|ConvertTo-Json))
    $archiveName="ruffle-libretro-v$version-windows-x86_64.zip"
    $archive=Join-Path $OutputDirectory $archiveName
    $files=@('ruffle_libretro.dll','ruffle_libretro.info','README.md','LICENSE.md','BUILD.json')|ForEach-Object{Join-Path $OutputDirectory $_}
    Compress-Archive -LiteralPath $files -DestinationPath $archive -Force
    $checksum=(Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    [IO.File]::WriteAllText("$archive.sha256","$checksum  $archiveName`n")
    $notes=@"
Ruffle libretro $version — Windows x64, OpenGL only, permanently offline.

Install ruffle_libretro.dll in RetroArch/cores and ruffle_libretro.info in RetroArch/info.
Targets RetroArch 1.7.5 and EmuVR. Stage3D and reconstruction savestates remain experimental.
Loading a state can take time; rewind, run-ahead and netplay are unsupported.

Source commit: $revision
Compiler: $compiler
"@
    [IO.File]::WriteAllText((Join-Path $OutputDirectory 'RELEASE_NOTES.md'),$notes)
    Write-Output $archive
} finally {
    $env:RUFFLE_TEST_SDL2=$oldSdl
    Pop-Location
}

[CmdletBinding()]
param(
    [switch]$SkipBuild,
    [ValidateSet("x86_64-pc-windows-msvc")]
    [string]$Target = "x86_64-pc-windows-msvc",
    [ValidateLength(1, 64)]
    [ValidatePattern('\A(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?(?:\+[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?\z')]
    [string]$Version = "0.1.0"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Resolve-ContainedPath {
    param(
        [Parameter(Mandatory)]
        [string]$ParentDirectory,
        [Parameter(Mandatory)]
        [string]$CandidatePath
    )

    $CanonicalParent = [System.IO.Path]::GetFullPath($ParentDirectory)
    $CanonicalCandidate = [System.IO.Path]::GetFullPath($CandidatePath)
    $TrimCharacters = [char[]]@(
        [System.IO.Path]::DirectorySeparatorChar,
        [System.IO.Path]::AltDirectorySeparatorChar
    )
    $CanonicalParent = $CanonicalParent.TrimEnd($TrimCharacters)
    $CandidateParent = [System.IO.Path]::GetDirectoryName($CanonicalCandidate)
    if (-not [string]::Equals(
            $CanonicalParent,
            $CandidateParent,
            [System.StringComparison]::OrdinalIgnoreCase
        )) {
        throw "Package path escapes the distribution directory: $CanonicalCandidate"
    }
    return $CanonicalCandidate
}

$RepositoryRoot = Split-Path -Parent $PSScriptRoot
$ReleaseDirectory = Join-Path $RepositoryRoot "target/$Target/release"
$DistributionDirectory = [System.IO.Path]::GetFullPath((Join-Path $RepositoryRoot "dist"))
$PackageName = "oxide-ide-v$Version-windows-x86_64"
$StagingDirectory = Resolve-ContainedPath $DistributionDirectory (
    Join-Path $DistributionDirectory $PackageName
)
$ArchivePath = Resolve-ContainedPath $DistributionDirectory (
    Join-Path $DistributionDirectory "$PackageName.zip"
)
$ChecksumPath = Resolve-ContainedPath $DistributionDirectory "$ArchivePath.sha256"

Push-Location $RepositoryRoot
try {
    if (-not $SkipBuild) {
        cargo build --locked --release --workspace --target $Target
        if ($LASTEXITCODE -ne 0) {
            throw "The Windows release build failed with exit code $LASTEXITCODE."
        }
    }

    $RequiredFiles = @(
        (Join-Path $ReleaseDirectory "oxide-ide.exe"),
        (Join-Path $ReleaseDirectory "rlox.exe"),
        (Join-Path $RepositoryRoot "LICENSE")
    )
    foreach ($Path in $RequiredFiles) {
        if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
            throw "Required release file is missing: $Path"
        }
    }

    New-Item -ItemType Directory -Path $DistributionDirectory -Force | Out-Null
    if (Test-Path -LiteralPath $StagingDirectory) {
        Remove-Item -LiteralPath $StagingDirectory -Recurse -Force
    }
    New-Item -ItemType Directory -Path $StagingDirectory | Out-Null

    Copy-Item -LiteralPath $RequiredFiles[0] -Destination $StagingDirectory
    Copy-Item -LiteralPath $RequiredFiles[1] -Destination $StagingDirectory
    Copy-Item -LiteralPath $RequiredFiles[2] -Destination $StagingDirectory

    if (Test-Path -LiteralPath $ArchivePath) {
        Remove-Item -LiteralPath $ArchivePath -Force
    }
    Compress-Archive -LiteralPath $StagingDirectory -DestinationPath $ArchivePath -CompressionLevel Optimal

    $Hash = (Get-FileHash -LiteralPath $ArchivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    "$Hash  $([System.IO.Path]::GetFileName($ArchivePath))" |
        Set-Content -LiteralPath $ChecksumPath -Encoding ascii -NoNewline
}
finally {
    if (Test-Path -LiteralPath $StagingDirectory) {
        Remove-Item -LiteralPath $StagingDirectory -Recurse -Force
    }
    Pop-Location
}

Write-Output $ArchivePath
Write-Output $ChecksumPath

$ErrorActionPreference = 'Stop'
$letter = ([char[]]'RSTUVWXYZ' | Where-Object { -not (Test-Path "${_}:\") } | Select-Object -First 1)
if (-not $letter) { throw 'No unused drive letter for the disposable test disk' }
$vhd = Join-Path $env:RUNNER_TEMP 'parallax-pressure.vhd'
$script = Join-Path $env:RUNNER_TEMP 'parallax-diskpart.txt'
$root = "${letter}:\"
$oldTemp = $env:TEMP
$oldTmp = $env:TMP
try {
    @"
create vdisk file="$vhd" maximum=128 type=expandable
select vdisk file="$vhd"
attach vdisk
create partition primary
format fs=ntfs quick label=ParallaxPressure
assign letter=$letter
"@ | Set-Content $script
    diskpart /s $script
    if (-not (Test-Path $root) -or (Get-Volume -DriveLetter $letter).FileSystemLabel -ne 'ParallaxPressure') {
        throw 'Disposable VHD was not mounted; refusing to fill any other filesystem'
    }
    [IO.File]::WriteAllText((Join-Path $root 'parallax-pressure-volume.marker'), "disposable-parallax-test-volume`n")
    $env:PARALLAX_FULL_VOLUME = $root
    $env:PARALLAX_PRESSURE_WORK = Join-Path $env:RUNNER_TEMP 'parallax-pressure-work'
    $env:TEMP = $root
    $env:TMP = $root
    cargo test -p parallax-core --test storage_pressure real_disk_full_extraction_and_saves -- --ignored --exact --nocapture --test-threads=1
    if ($LASTEXITCODE -ne 0) { throw "Disk-full integration test failed ($LASTEXITCODE)" }
} finally {
    $env:TEMP = $oldTemp
    $env:TMP = $oldTmp
    @"
select vdisk file="$vhd"
detach vdisk
"@ | Set-Content $script
    diskpart /s $script
    Remove-Item $vhd -Force -ErrorAction SilentlyContinue
}

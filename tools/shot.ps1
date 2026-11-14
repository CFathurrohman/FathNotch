param(
    [Parameter(Mandatory)][string]$Tool,       # tool id, e.g. "todos" (or "alert")
    [Parameter(Mandatory)][string]$Out,        # PNG path
    [string]$Exe = "",
    [int]$WaitMs = 3500,
    [string]$DataDir = "",                     # optional scratch data folder (FATHNOTCH_DATA_DIR)
    [string]$Actions = ""                        # scripted input, e.g. "click:400,120;type:hello;key:Enter"
)
if (-not $Exe) { $Exe = (Resolve-Path "$PSScriptRoot\..\target\release\fathnotch.exe").Path }
# Renders FathNotch open on one tool to a PNG, off screen, without touching a running copy.
$env:FATHNOTCH_DEMO = $Tool
$env:FATHNOTCH_SHOT = (Resolve-Path -LiteralPath (Split-Path $Out -Parent)).Path + "\" + (Split-Path $Out -Leaf)
$env:FATHNOTCH_SHOT_WAIT = "$WaitMs"
if ($DataDir) { $env:FATHNOTCH_DATA_DIR = $DataDir }
if ($Actions) { $env:FATHNOTCH_SHOT_INPUT = $Actions }
try {
    $p = Start-Process $Exe -PassThru
    if (-not $p.WaitForExit($WaitMs + 15000)) { Stop-Process -Id $p.Id -Force; Write-Error "timed out" }
} finally {
    Remove-Item Env:FATHNOTCH_DEMO, Env:FATHNOTCH_SHOT, Env:FATHNOTCH_SHOT_WAIT, Env:FATHNOTCH_SHOT_INPUT -ErrorAction SilentlyContinue
    if ($DataDir) { Remove-Item Env:FATHNOTCH_DATA_DIR -ErrorAction SilentlyContinue }
}
if (Test-Path $Out) { "saved $Out" } else { Write-Error "no image written" }




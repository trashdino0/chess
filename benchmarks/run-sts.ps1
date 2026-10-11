# STS harness (P6): fixed-depth solve rate over benchmarks/sts.epd.
# Drives the deterministic `search` CLI (single-threaded, fresh process and
# fresh TT per position — no cross-position pollution). Per-theme + total
# solve rates give per-commit component scores that self-play Elo (±63 at
# 200 games) is too noisy for. No engine change; rerun after every commit
# that touches search/eval and log the delta in the commit message.
# Usage: .\benchmarks\run-sts.ps1 [-Exe .\target\release\dinochess.exe] [-Epd .\benchmarks\sts.epd] [-Depth 8]
param([string]$Exe = ".\target\release\dinochess.exe", [string]$Epd = ".\benchmarks\sts.epd", [int]$Depth = 8)

$lines = Get-Content $Epd | Where-Object { $_.Trim() -ne "" -and -not $_.Trim().StartsWith("#") }
$themes = @{}
$totalSolve = 0
$totalN = 0
$misses = @()
$t = [System.Diagnostics.Stopwatch]::StartNew()
foreach ($line in $lines) {
    $toks = $line -split '\s+'
    $fen = ($toks[0..5] -join ' ')
    $ops = $line.Substring($fen.Length)
    $bm = @()
    $am = @()
    $id = "?"
    foreach ($part in ($ops -split ';')) {
        $p = $part.Trim()
        if ($p -eq "") { continue }
        $key, $rest = $p -split '\s+', 2
        if ($key -eq "bm") { $bm = @($rest -split '\s+') }
        elseif ($key -eq "am") { $am = @($rest -split '\s+') }
        elseif ($key -eq "id") { $id = $rest }
    }
    $theme = ($id -split '\.')[0]
    $out = & $Exe search $Depth $fen 2>&1 | Select-Object -Last 1
    $got = ""
    $score = 0
    if ($out -match '^bestmove (\S+)(?: score (-?\d+))?') {
        $got = $Matches[1]
        if ($Matches[2]) { $score = [int]$Matches[2] }
    }
    $isMate = [Math]::Abs($score) -ge 99000
    if ($bm.Count -gt 0) {
        $solved = ($bm -contains $got) -or $isMate
    } else {
        $solved = ($am -notcontains $got) -or $isMate
    }
    if (-not $themes.ContainsKey($theme)) { $themes[$theme] = @{ Solve = 0; N = 0 } }
    $themes[$theme].N++
    $totalN++
    if ($solved) {
        $themes[$theme].Solve++
        $totalSolve++
    } else {
        $misses += ("MISS {0} got={1} score={2} want=[{3}] avoid=[{4}]" -f $id, $got, $score, ($bm -join ','), ($am -join ','))
    }
}
$t.Stop()
foreach ($m in $misses) { Write-Output $m }
foreach ($th in ($themes.Keys | Sort-Object)) {
    Write-Output ("{0} {1}/{2}" -f $th, $themes[$th].Solve, $themes[$th].N)
}
Write-Output ("TOTAL {0}/{1} depth={2} time={3}s" -f $totalSolve, $totalN, $Depth, [math]::Round($t.Elapsed.TotalSeconds, 1))

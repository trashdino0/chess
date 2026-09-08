# Query a UCI engine at fixed depth, driven interactively.
# Piped stdin cannot be used for `go` searches (EOF/quit aborts mid-search),
# so this drives the process with redirected stdin/stdout instead.
param([string]$Exe, [string]$Fen, [int]$Depth = 8)

$p = New-Object System.Diagnostics.Process
$p.StartInfo.FileName = $Exe
$p.StartInfo.UseShellExecute = $false
$p.StartInfo.RedirectStandardInput = $true
$p.StartInfo.RedirectStandardOutput = $true
$p.StartInfo.CreateNoWindow = $true
[void]$p.Start()
$in = $p.StandardInput
$out = $p.StandardOutput
$lastInfo = ""
function Wait-For([string]$prefix) {
  while ($true) {
    $line = $out.ReadLine()
    if ($line -eq $null) { return $null }
    if ($line.StartsWith($prefix)) { return $line }
    if ($prefix -eq "bestmove" -and $line.StartsWith("info ")) { $script:lastInfo = $line }
  }
}
$in.WriteLine("uci"); [void](Wait-For "uciok")
$in.WriteLine("isready"); [void](Wait-For "readyok")
$in.WriteLine("position fen $Fen")
$in.WriteLine("go depth $Depth")
$bm = Wait-For "bestmove"
$script:lastInfo
$bm
$in.WriteLine("quit")
[void]$p.WaitForExit(5000)

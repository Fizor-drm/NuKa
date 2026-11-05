# Start llama-server and the translation bridge.
# Messages stay ASCII so Windows PowerShell 5.1 can parse this file without a BOM.

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$runtime = Join-Path $root "runtime"
$model = Join-Path $runtime "models\plamo-3-translate-0.8b-Q4_K_M.gguf"
$server = Join-Path $runtime "llama\llama-server.exe"

if (-not (Test-Path $model)) {
  Write-Error "Model missing. Run scripts\fetch-runtime.ps1 first: $model"
}
if (-not (Test-Path $server)) {
  Write-Error "llama-server missing. Run scripts\fetch-runtime.ps1 first: $server"
}

$llamaPort = if ($env:PLAMO_LLAMA_PORT) { $env:PLAMO_LLAMA_PORT } else { "8090" }
$env:PLAMO_UPSTREAM = "http://127.0.0.1:$llamaPort"

Write-Host "starting llama-server on 127.0.0.1:$llamaPort"
$llamaDir = Split-Path -Parent $server
$llamaLog = Join-Path $runtime "llama-server.log"
$llamaErr = Join-Path $runtime "llama-server.err.log"
# Windows PowerShell joins an argument array and splits it again, so a path
# containing spaces must be one quoted string.
$llamaArgs = "-m `"$model`" --host 127.0.0.1 --port $llamaPort -c 8192 -ngl all --alias plamo-3-translate --no-webui"
$llama = Start-Process -FilePath $server -WorkingDirectory $llamaDir -ArgumentList $llamaArgs -PassThru -WindowStyle Hidden -RedirectStandardOutput $llamaLog -RedirectStandardError $llamaErr

try {
  $ready = $false
  foreach ($attempt in 1..150) {
    Start-Sleep -Seconds 2
    if ($llama.HasExited) {
      throw "llama-server exited with code $($llama.ExitCode). See runtime\llama-server.err.log"
    }
    try {
      $health = Invoke-WebRequest -Uri "http://127.0.0.1:${llamaPort}/health" -UseBasicParsing -TimeoutSec 3
      if ($health.StatusCode -eq 200) { $ready = $true; break }
    } catch {
      Write-Host "loading model ($attempt)"
    }
  }
  if (-not $ready) { throw "llama-server did not answer within 5 minutes. See runtime\llama-server.err.log" }

  Write-Host "starting bridge on 127.0.0.1:8788. Press Ctrl+C to stop."
  py -3 (Join-Path $root "bridge\server.py")
} finally {
  if (-not $llama.HasExited) {
    Stop-Process -Id $llama.Id -Force
    Write-Host "stopped llama-server"
  }
}

# llama.cpp の Windows CUDA 12.4 ビルドと、PLaMo 3 Translate 0.8B の GGUF Q4_K_M を runtime/ に置く。
# GTX 1660 (Turing, 6GB) では CUDA 12.4 ビルドが動く。13.x は Ada 以降向けなので使わない。
# モデルは PLaMo Community License。個人利用と非商用は登録なしで使える。商用は LICENSE の案内を読む。
#
# 落としたファイルは、次の 3 点で確かめてから使う。
#   1. 展開先の llama-server と CUDA の DLL がそろっているか
#   2. モデルのサイズが配布元の Content-Length と一致するか
#   3. 途中で止まった残骸 (.part) は使わない

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$runtime = Join-Path $root "runtime"
$llamaDir = Join-Path $runtime "llama"
$modelDir = Join-Path $runtime "models"
New-Item -ItemType Directory -Force -Path $llamaDir, $modelDir | Out-Null

$tag = "b12136"
$llamaZip = Join-Path $runtime "llama-$tag-bin-win-cuda-12.4-x64.zip"
$cudartZip = Join-Path $runtime "cudart-llama-bin-win-cuda-12.4-x64.zip"
$llamaUrl = "https://github.com/ggml-org/llama.cpp/releases/download/$tag/llama-$tag-bin-win-cuda-12.4-x64.zip"
$cudartUrl = "https://github.com/ggml-org/llama.cpp/releases/download/$tag/cudart-llama-bin-win-cuda-12.4-x64.zip"
$modelUrl = "https://huggingface.co/pfnet/plamo-3-translate-0.8b-GGUF/resolve/main/plamo-3-translate-0.8b-Q4_K_M.gguf"
$model = Join-Path $modelDir "plamo-3-translate-0.8b-Q4_K_M.gguf"

function Get-RemoteSize($url) {
  $head = curl.exe -sIL --fail $url
  if ($LASTEXITCODE -ne 0) { throw "cannot read headers: $url" }
  $length = $head | Select-String -Pattern '^content-length:\s*(\d+)' -AllMatches |
    ForEach-Object { [int64]$_.Matches[0].Groups[1].Value } | Select-Object -Last 1
  return $length
}

function Get-File($url, $dest) {
  $expected = Get-RemoteSize $url
  if (Test-Path $dest) {
    $actual = (Get-Item $dest).Length
    if ($expected -and $actual -eq $expected) {
      Write-Host "ok $dest"
      return
    }
    Write-Host "size mismatch, downloading again: $dest ($actual / $expected)"
    Remove-Item $dest -Force
  }
  $part = "$dest.part"
  if (Test-Path $part) { Remove-Item $part -Force }
  Write-Host "get $url"
  curl.exe -L --fail --retry 3 -o $part $url
  if ($LASTEXITCODE -ne 0) { throw "download failed: $url" }
  if ($expected -and (Get-Item $part).Length -ne $expected) {
    Remove-Item $part -Force
    throw "download size mismatch: $url"
  }
  Move-Item $part $dest -Force
}

Get-File $llamaUrl $llamaZip
Get-File $cudartUrl $cudartZip

# 展開は毎回やり直す。途中で止まったときに DLL だけ欠けるのを防ぐ。
Write-Host "expand llama.cpp"
Expand-Archive -Path $llamaZip -DestinationPath $llamaDir -Force
Expand-Archive -Path $cudartZip -DestinationPath $llamaDir -Force

foreach ($required in "llama-server.exe", "ggml-cuda.dll", "cudart64_12.dll", "cublas64_12.dll") {
  if (-not (Test-Path (Join-Path $llamaDir $required))) {
    throw "missing after expand: $required"
  }
}

Get-File $modelUrl $model

Write-Host "runtime ready"
Get-Item (Join-Path $llamaDir "llama-server.exe"), $model | Select-Object FullName, Length | Format-List

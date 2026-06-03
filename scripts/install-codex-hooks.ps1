param(
  [string]$Root = (Get-Location).Path,
  [string]$HookCommand = "",
  [switch]$WhatIfOnly
)

$ErrorActionPreference = "Stop"
$rootPath = (Resolve-Path -LiteralPath $Root).Path
$hookExe = Join-Path $rootPath "target\debug\agentcall-hook.exe"
if ([string]::IsNullOrWhiteSpace($HookCommand)) {
  if (-not (Test-Path -LiteralPath $hookExe)) {
    throw "Rust hook binary not found: $hookExe. Run: cargo build -p agentcall-hook"
  }
  $HookCommand = $hookExe
}

$codexDir = Join-Path $rootPath ".codex"
$hooksPath = Join-Path $codexDir "hooks.json"
New-Item -ItemType Directory -Path $codexDir -Force | Out-Null

function New-Hook($eventName, $statusMessage, $matcher = $null) {
  $escapedHook = $HookCommand.Replace("'", "''")
  $escapedRoot = $rootPath.Replace("'", "''")
  $command = "powershell.exe -NoProfile -ExecutionPolicy Bypass -Command `"& '$escapedHook' --root '$escapedRoot' --event $eventName --runtime codex`""
  $entry = [ordered]@{
    hooks = @(
      [ordered]@{
        type = "command"
        command = $command
        commandWindows = $command
        timeout = 10
        statusMessage = $statusMessage
      }
    )
  }
  if ($matcher) {
    $entry["matcher"] = $matcher
  }
  return $entry
}

$config = [ordered]@{
  hooks = [ordered]@{
    SessionStart = @((New-Hook "SessionStart" "AgentCall: loading workspace state" "startup|resume"))
    UserPromptSubmit = @((New-Hook "UserPromptSubmit" "AgentCall: checking orchestration state"))
    Stop = @((New-Hook "Stop" "AgentCall: recording stop state"))
    PreCompact = @((New-Hook "PreCompact" "AgentCall: saving pre-compact state"))
    PostCompact = @((New-Hook "PostCompact" "AgentCall: restoring orchestration hints"))
  }
}

$json = $config | ConvertTo-Json -Depth 20
if ($WhatIfOnly) {
  $json
  exit 0
}

$json | Set-Content -LiteralPath $hooksPath -Encoding UTF8
Write-Output "Installed AgentCall Codex hooks: $hooksPath"
Write-Output "Open a new Codex session or use the app hook trust flow if prompted."

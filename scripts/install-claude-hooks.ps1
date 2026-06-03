param(
  [string]$Root = (Get-Location).Path,
  [string]$Python = "python",
  [ValidateSet("project-local", "project", "user")]
  [string]$Scope = "project-local",
  [switch]$WhatIfOnly
)

$ErrorActionPreference = "Stop"
$rootPath = (Resolve-Path -LiteralPath $Root).Path
$hookScript = Join-Path $rootPath "scripts\agentcall-claude-hook.py"
if (-not (Test-Path -LiteralPath $hookScript)) {
  throw "Hook script not found: $hookScript"
}

if ($Scope -eq "user") {
  $settingsPath = Join-Path $HOME ".claude\settings.json"
} elseif ($Scope -eq "project") {
  $settingsPath = Join-Path $rootPath ".claude\settings.json"
} else {
  $settingsPath = Join-Path $rootPath ".claude\settings.local.json"
}

$settingsDir = Split-Path -Parent $settingsPath
New-Item -ItemType Directory -Path $settingsDir -Force | Out-Null

if (Test-Path -LiteralPath $settingsPath) {
  $raw = Get-Content -LiteralPath $settingsPath -Raw -Encoding UTF8
  if ([string]::IsNullOrWhiteSpace($raw)) {
    $settings = [ordered]@{}
  } else {
    $settings = $raw | ConvertFrom-Json -AsHashtable
  }
} else {
  $settings = [ordered]@{}
}

if (-not $settings.Contains("hooks") -or $null -eq $settings["hooks"]) {
  $settings["hooks"] = [ordered]@{}
}

$events = @(
  @{ Name = "SessionStart"; Matcher = $null },
  @{ Name = "UserPromptSubmit"; Matcher = $null },
  @{ Name = "PreToolUse"; Matcher = "*" },
  @{ Name = "PostToolUse"; Matcher = "*" },
  @{ Name = "Notification"; Matcher = $null },
  @{ Name = "Stop"; Matcher = $null },
  @{ Name = "SubagentStop"; Matcher = $null },
  @{ Name = "PreCompact"; Matcher = $null },
  @{ Name = "SessionEnd"; Matcher = $null }
)

foreach ($event in $events) {
  $entry = [ordered]@{
    hooks = @(
      [ordered]@{
        type = "command"
        command = $Python
        args = @(
          $hookScript,
          "--root",
          $rootPath,
          "--event",
          $event.Name,
          "--python",
          $Python
        )
        timeout = 30
      }
    )
  }
  if ($event.Matcher) {
    $entry["matcher"] = $event.Matcher
  }
  $settings["hooks"][$event.Name] = @($entry)
}

$json = $settings | ConvertTo-Json -Depth 20
if ($WhatIfOnly) {
  $json
  exit 0
}

$json | Set-Content -LiteralPath $settingsPath -Encoding UTF8
Write-Output "Installed AgentCall Claude Code hooks: $settingsPath"
Write-Output "Use /hooks inside Claude Code to inspect the loaded project/local hooks."

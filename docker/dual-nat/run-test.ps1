param(
    [switch]$NoBuild
)

$ErrorActionPreference = "Stop"
$composeFile = Join-Path $PSScriptRoot "docker-compose.yml"

if (-not $NoBuild) {
    docker compose -f $composeFile build
}

docker compose -f $composeFile up --abort-on-container-exit --exit-code-from peer_a
$code = $LASTEXITCODE

docker compose -f $composeFile down --remove-orphans

exit $code

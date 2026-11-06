#!/usr/bin/env bash
# KYB (Know Your Business) installer: the server, the kyb CLI and the agent skill.
# Copyright (c) 2026 Alexander Panasenko
#
# Contact: alex@prod.codes
# Author: https://prod.codes/about/
# Project: https://github.com/alex09x/kyb
# SPDX-License-Identifier: MIT
#
#   curl -fsSL https://kybmemory.com/install.sh | bash
#
# It looks at the machine and picks one way to run the server:
#   macOS on Apple Silicon with Homebrew  -> brew install alex09x/apps/kyb + brew services
#   macOS on Apple Silicon without it      -> the release archive (SHA-256 checked) + launchd
#   anything with Docker (Linux, Intel Mac) -> ghcr.io/alex09x/kyb in ~/kyb via docker compose
#   a server already answering :9310 here  -> leaves it alone (one this script
#                                             installed is upgraded instead)
# and then installs the kyb CLI (~/.local/bin/kyb) and the skill for Claude Code,
# Codex and Antigravity (whichever are present), pointed at that server.
#
# Settings (environment, all optional):
#   KYB_MODE=auto|brew|native|docker|client
#                                      native = macOS release archive under launchd, no Homebrew;
#                                      client = only the CLI and skill, for a server elsewhere
#   KYB_SERVER=your-server             where the CLI points (client mode; default this machine)
#   KYB_VERSION=v0.3.3                 release to install (default: the latest release)
#   KYB_DIR=~/kyb                      Docker and native: where the data (and compose files) live
#   KYB_PORT=9310                      Docker and native: port on 127.0.0.1
#   KYB_DRY_RUN=1                      print the plan and change nothing
#
# Pass them to bash, not to curl:  curl -fsSL .../install.sh | KYB_MODE=client KYB_SERVER=your-server bash
set -euo pipefail

# Everything runs from main, called on the last line, so a download cut short
# runs nothing at all instead of half an installer.
KYB_REPO="${KYB_REPO:-alex09x/kyb}"
KYB_SITE="${KYB_SITE:-https://kybmemory.com}"
KYB_MODE="${KYB_MODE:-auto}"
KYB_DIR="${KYB_DIR:-$HOME/kyb}"
KYB_PORT="${KYB_PORT:-9310}"
KYB_DRY_RUN="${KYB_DRY_RUN:-}"
KYB_PLIST="${KYB_PLIST:-$HOME/Library/LaunchAgents/com.kybmemory.kyb-server.plist}"
KYB_LABEL=com.kybmemory.kyb-server

say()  { printf '\033[1m==>\033[0m %s\n' "$*"; }
ok()   { printf '  \342\234\223 %s\n' "$*"; }
die()  { printf 'kyb install: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

detect_os() {
  case "${KYB_OS:-$(uname -s)}" in
    Darwin) echo macos ;;
    Linux)  echo linux ;;
    *)      echo other ;;
  esac
}

detect_arch() {
  case "${KYB_ARCH:-$(uname -m)}" in
    arm64|aarch64) echo arm64 ;;
    x86_64|amd64)  echo amd64 ;;
    *)             echo other ;;
  esac
}

# The Compose command this machine has, or nothing.
compose_cmd() {
  if docker compose version >/dev/null 2>&1; then echo "docker compose"
  elif have docker-compose; then echo "docker-compose"
  fi
}

docker_usable() { have docker && docker info >/dev/null 2>&1 && [ -n "$(compose_cmd)" ]; }

# Rootless Docker maps container root to the host user and every other container
# uid to a subordinate one, so ./data is writable only as uid 0 inside.
docker_rootless() { docker info --format '{{.SecurityOptions}}' 2>/dev/null | grep -q rootless; }

server_answers() { curl -fsS --max-time 3 "http://127.0.0.1:$KYB_PORT/healthz" >/dev/null 2>&1; }

# Which way to run the server here. Prints brew, native, docker, existing or client.
choose_mode() {
  local os="$1" arch="$2"
  case "$KYB_MODE" in
    brew|native|docker|client) echo "$KYB_MODE"; return ;;
    auto) ;;
    *) die "KYB_MODE must be auto, brew, native, docker or client, not '$KYB_MODE'" ;;
  esac
  # A server this installer set up before is upgraded in place, even while it runs.
  if [ "$os" = macos ] && have brew && brew list --formula kyb >/dev/null 2>&1; then echo brew; return; fi
  if [ "$os" = macos ] && [ -f "$KYB_PLIST" ]; then echo native; return; fi
  if [ -f "$KYB_DIR/docker-compose.yml" ] && docker_usable; then echo docker; return; fi
  if server_answers; then echo existing; return; fi
  if [ "$os" = macos ] && [ "$arch" = arm64 ]; then
    if have brew; then echo brew; else echo native; fi
    return
  fi
  if docker_usable; then echo docker; return; fi
  if have docker; then
    die "Docker is installed but not usable here (try: docker info). Start Docker if it is stopped;
on Linux add yourself to the docker group (sudo usermod -aG docker \$USER, then log in again), or install Docker Compose v2, and run this again."
  fi
  case "$os" in
    macos)
      die "Intel macOS has no native build. Install Docker Desktop and run this again." ;;
    linux)
      die "the server runs in Docker on Linux. Install Docker Engine with the Compose plugin (https://docs.docker.com/engine/install/) and run this again." ;;
    *)
      die "unsupported system '$(uname -s)'. Run the server from Docker, or install only the CLI: KYB_MODE=client KYB_SERVER=<host>." ;;
  esac
}

# The release tag to install: KYB_VERSION, or the latest GitHub release.
resolve_version() {
  local v="${KYB_VERSION:-}"
  if [ -z "$v" ]; then
    # The /releases/latest page redirects to the tag; unlike the API it has no
    # per-address rate limit, which machines behind one NAT (or CI runners) hit.
    v="$(curl -fsSI -o /dev/null -w '%{redirect_url}' "https://github.com/$KYB_REPO/releases/latest" \
          | sed -n 's|.*/releases/tag/\([^/?#]*\).*|\1|p')" \
      || die "could not ask GitHub for the latest release; set KYB_VERSION=vX.Y.Z"
    [ -n "$v" ] || die "could not read the latest release; set KYB_VERSION=vX.Y.Z"
  fi
  case "$v" in v*) ;; *) v="v$v" ;; esac
  echo "$v"
}

wait_healthy() {
  for _ in $(seq 1 60); do
    server_answers && return 0
    sleep 1
  done
  return 1
}

install_brew() {
  say "server: Homebrew (alex09x/apps/kyb)"
  if brew list --formula kyb >/dev/null 2>&1; then
    brew upgrade kyb || true
  else
    brew install alex09x/apps/kyb
  fi
  brew services restart kyb >/dev/null
  wait_healthy || die "the kyb service did not answer on 127.0.0.1:$KYB_PORT; see $(brew --prefix)/var/log/kyb.log"
  ok "kyb-server $(brew list --versions kyb | awk '{print $2}') running as a Homebrew service"
}

# macOS without Homebrew: the release archive, checked against the SHA-256 digest
# GitHub records for the asset, binaries in ~/.local/bin, the server under launchd.
install_native() {
  local version="$1" asset tmp want got src uid
  asset="kyb-$version-aarch64-apple-darwin.tar.gz"
  if ! { have python3 && python3 -c 1 >/dev/null 2>&1; }; then
    die "the kyb CLI needs python3 (xcode-select --install); install it and run this again"
  fi
  say "server: release archive $asset under launchd"
  tmp="$(mktemp -d)"
  want="$(github_api "repos/$KYB_REPO/releases/tags/$version" | ASSET="$asset" python3 -c '
import json, os, sys
for a in json.load(sys.stdin).get("assets", []):
    if a.get("name") == os.environ["ASSET"] and str(a.get("digest", "")).startswith("sha256:"):
        print(a["digest"].split(":", 1)[1])
')" || { rm -rf "$tmp"; die "could not read the $version release from the GitHub API (rate limited? set GITHUB_TOKEN, or retry later)"; }
  [ -n "$want" ] || { rm -rf "$tmp"; die "the $version release has no $asset with a SHA-256 digest"; }
  curl -fsSL -o "$tmp/$asset" "https://github.com/$KYB_REPO/releases/download/$version/$asset" \
    || { rm -rf "$tmp"; die "could not download $asset"; }
  got="$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')"
  [ "$got" = "$want" ] || { rm -rf "$tmp"; die "$asset SHA-256 is $got, GitHub records $want; not installing"; }
  ok "$asset SHA-256 matches the release"
  tar -xzf "$tmp/$asset" -C "$tmp"
  src="$tmp/kyb-$version-aarch64-apple-darwin"
  install -d "$HOME/.local/bin" "$KYB_DIR/data" "$(dirname "$KYB_PLIST")"
  install -m 755 "$src/bin/kyb-server" "$HOME/.local/bin/kyb-server"
  cat > "$KYB_PLIST" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$KYB_LABEL</string>
  <key>ProgramArguments</key><array><string>$HOME/.local/bin/kyb-server</string></array>
  <key>WorkingDirectory</key><string>$KYB_DIR</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>KYB_DATA</key><string>$KYB_DIR/data/kyb-data</string>
    <key>KYB_INDEX</key><string>$KYB_DIR/data/index</string>
    <key>KYB_AUDIT</key><string>$KYB_DIR/data/audit.jsonl</string>
    <key>KYB_AUTH_FILE</key><string>$KYB_DIR/data/auth.json</string>
    <key>KYB_ADDR</key><string>127.0.0.1:$KYB_PORT</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>$KYB_DIR/kyb.log</string>
  <key>StandardErrorPath</key><string>$KYB_DIR/kyb.log</string>
</dict>
</plist>
PLIST
  uid="$(id -u)"
  # bootout ends the running server (an upgrade), then bootstrap starts the new binary.
  launchctl bootout "gui/$uid/$KYB_LABEL" >/dev/null 2>&1 || true
  launchctl bootstrap "gui/$uid" "$KYB_PLIST" || { rm -rf "$tmp"; die "launchctl could not load $KYB_PLIST"; }
  if ! wait_healthy; then
    tail -n 20 "$KYB_DIR/kyb.log" >&2 || true
    rm -rf "$tmp"; die "kyb-server did not answer on 127.0.0.1:$KYB_PORT; log above ($KYB_DIR/kyb.log)"
  fi
  ok "kyb-server $version running under launchd ($KYB_LABEL); data in $KYB_DIR/data"
  say "CLI and agent skill ($version, from the same archive)"
  KYB_SERVER="127.0.0.1" KYB_INSTALL_BINARY="$src/bin/kyb" bash "$src/skills/install.sh" \
    || { rm -rf "$tmp"; die "skills/install.sh failed"; }
  rm -rf "$tmp"
}

# GitHub API, with a token when the environment has one (GH_TOKEN or GITHUB_TOKEN):
# without it GitHub allows 60 requests an hour per address.
github_api() {
  local token="${GH_TOKEN:-${GITHUB_TOKEN:-}}"
  if [ -n "$token" ]; then
    curl -fsSL -H "Authorization: Bearer $token" "https://api.github.com/$1"
  else
    curl -fsSL "https://api.github.com/$1"
  fi
}

# Sets KEY=VALUE in an env file, keeping every other line as the user left it.
set_env() {
  local file="$1" key="$2" value="$3" tmp
  tmp="$(mktemp)"
  : > "$tmp"
  if [ -f "$file" ]; then grep -v "^$key=" "$file" > "$tmp" || true; fi
  printf '%s=%s\n' "$key" "$value" >> "$tmp"
  mv "$tmp" "$file"
}

install_docker() {
  local version="$1" compose
  compose="$(compose_cmd)"
  say "server: Docker (ghcr.io/alex09x/kyb:$version) in $KYB_DIR"
  mkdir -p "$KYB_DIR/data"
  curl -fsSL "$KYB_SITE/docker-compose.yml" -o "$KYB_DIR/docker-compose.yml.new" \
    || die "could not download $KYB_SITE/docker-compose.yml"
  mv "$KYB_DIR/docker-compose.yml.new" "$KYB_DIR/docker-compose.yml"
  set_env "$KYB_DIR/.env" KYB_VERSION "$version"
  if docker_rootless; then
    set_env "$KYB_DIR/.env" KYB_UID 0
    set_env "$KYB_DIR/.env" KYB_GID 0
  else
    set_env "$KYB_DIR/.env" KYB_UID "$(id -u)"
    set_env "$KYB_DIR/.env" KYB_GID "$(id -g)"
  fi
  set_env "$KYB_DIR/.env" KYB_PORT "$KYB_PORT"
  ok "$KYB_DIR/docker-compose.yml and .env (your other .env lines are kept)"
  (cd "$KYB_DIR" && $compose pull --quiet && $compose up -d) || die "docker compose could not start the server"
  if ! wait_healthy; then
    (cd "$KYB_DIR" && $compose logs --tail 20) >&2 || true
    die "the container did not answer on 127.0.0.1:$KYB_PORT; logs above (cd $KYB_DIR && $compose logs)"
  fi
  ok "kyb-server $version running; data in $KYB_DIR/data"
}

# The CLI and the skill come from the release's source archive: skills/install.sh
# puts the CLI in ~/.local/bin and the skill into every agent found.
install_client() {
  local version="$1" server="${2:-}" tmp src
  have python3 || die "the kyb CLI needs python3; install it and run this again"
  say "CLI and agent skill ($version)"
  tmp="$(mktemp -d)"
  curl -fsSL "https://github.com/$KYB_REPO/archive/refs/tags/$version.tar.gz" | tar -xz -C "$tmp" \
    || { rm -rf "$tmp"; die "could not download the $version source archive"; }
  src="$(find "$tmp" -mindepth 1 -maxdepth 1 -type d | head -n1)"
  [ -f "$src/skills/install.sh" ] || { rm -rf "$tmp"; die "the $version archive has no skills/install.sh"; }
  KYB_SERVER="$server" bash "$src/skills/install.sh" || { rm -rf "$tmp"; die "skills/install.sh failed"; }
  rm -rf "$tmp"
}

install_brew_client() {
  say "CLI and agent skill (from Homebrew)"
  local prefix
  prefix="$(brew --prefix)"
  KYB_SERVER="$1" KYB_INSTALL_BINARY="$prefix/bin/kyb" bash "$prefix/share/kyb/skills/install.sh"
}

main() {
  have curl || die "curl is required"
  have tar  || die "tar is required"
  local os arch mode version="" server=""
  os="$(detect_os)"; arch="$(detect_arch)"
  mode="$(choose_mode "$os" "$arch")"
  if [ "$mode" = docker ] || [ "$mode" = native ] || [ "$mode" = client ] || [ "$mode" = existing ]; then
    version="$(resolve_version)"
  fi
  # The CLI points at the server this run installed or found. A host file left
  # by an earlier install (another machine's address) would otherwise win.
  if [ "$mode" = client ]; then server="${KYB_SERVER:-}"; else server=127.0.0.1; fi

  say "plan: os=$os arch=$arch server=$mode${version:+ version=$version}${server:+ cli->$server}"
  [ -z "$KYB_DRY_RUN" ] || exit 0

  case "$mode" in
    brew)     install_brew; install_brew_client "$server" ;;
    native)   install_native "$version" ;;
    docker)   install_docker "$version"; install_client "$version" "$server" ;;
    existing) ok "a kyb server already answers on 127.0.0.1:$KYB_PORT; leaving it as it is"
              install_client "$version" "$server" ;;
    client)   install_client "$version" "$server" ;;
  esac

  echo
  case ":$PATH:" in
    *":$HOME/.local/bin:"*) ;;
    *) say "add ~/.local/bin to PATH:  export PATH=\"\$HOME/.local/bin:\$PATH\"" ;;
  esac
  if [ "$mode" != client ] && [ "$KYB_PORT" != 9310 ]; then
    say "the server listens on port $KYB_PORT: export KYB_ADDR=127.0.0.1:$KYB_PORT for the CLI"
  fi
  say "done. Try:  kyb health   then   kyb query \"what do we know\""
}

main "$@"

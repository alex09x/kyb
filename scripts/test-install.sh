#!/usr/bin/env bash
# Stub bodies are written for the stub to expand, not this shell.
# shellcheck disable=SC2016
# Checks how docs/install.sh (served as https://kybmemory.com/install.sh) chooses
# a way to install on each kind of machine. Every case is a dry run against stub
# brew/docker/curl on a bare PATH: nothing is installed and nothing is fetched.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
installer="$repo_root/docs/install.sh"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
fail=0

# stub <dir> <name> <script body>
stub() { mkdir -p "$1"; printf '#!/bin/sh\n%s\n' "$3" > "$1/$2"; chmod +x "$1/$2"; }

# The PATH of every case holds only these tools, linked from the real system,
# so a brew or docker installed on the machine running the test is never seen.
tools="$work/tools"; mkdir -p "$tools"
for t in bash sh env uname sed grep head cat id mktemp mv seq sleep tr awk tar; do
  real="$(command -v "$t")" || { echo "FAIL the test needs $t"; exit 1; }
  ln -s "$real" "$tools/$t"
done
# curl is needed only for the health probe, which these cases must not reach:
# by default it is a stub that fails, as if no server listened.
base="$work/base"
stub "$base" curl 'exit 7'

# case <name> <expected exit> <expected text> <stub dir> [VAR=value ...]
case_() {
  local name="$1" want_rc="$2" want="$3" dir="$4"; shift 4
  local out rc=0
  out="$(env -i HOME="$work/home" PATH="$dir:$base:$tools" KYB_VERSION=v0.3.3 \
         KYB_DRY_RUN=1 "$@" bash "$installer" 2>&1)" || rc=$?
  if [ "$rc" = "$want_rc" ] && printf '%s' "$out" | grep -qF -- "$want"; then
    echo "ok   $name"
  else
    echo "FAIL $name: exit $rc (want $want_rc), output:"; printf '%s\n' "$out" | sed 's/^/     /'
    fail=1
  fi
}

none="$work/none"; mkdir -p "$none"
brew_only="$work/brew"; stub "$brew_only" brew 'exit 0'
docker_ok="$work/docker-ok"; stub "$docker_ok" docker 'exit 0'
docker_down="$work/docker-down"
stub "$docker_down" docker 'case "$1" in info) exit 1;; *) exit 0;; esac'
both="$work/both"; stub "$both" brew 'exit 0'; stub "$both" docker 'exit 0'
answering="$work/answering"; stub "$answering" curl 'exit 0'
brew_none="$work/brew-none"; stub "$brew_none" brew 'case "$1" in list) exit 1;; *) exit 0;; esac'

case_ "Apple Silicon with Homebrew uses brew" 0 "server=brew" \
  "$brew_only" KYB_OS=Darwin KYB_ARCH=arm64
case_ "Apple Silicon prefers brew over Docker" 0 "server=brew" \
  "$both" KYB_OS=Darwin KYB_ARCH=arm64
case_ "Apple Silicon without brew installs the release natively" 0 "server=native version=v0.3.3" \
  "$docker_ok" KYB_OS=Darwin KYB_ARCH=arm64
case_ "Intel Mac with Docker uses Docker" 0 "server=docker" \
  "$docker_ok" KYB_OS=Darwin KYB_ARCH=x86_64
case_ "Linux amd64 with Docker uses Docker" 0 "os=linux arch=amd64 server=docker" \
  "$docker_ok" KYB_OS=Linux KYB_ARCH=x86_64
case_ "Linux arm64 with Docker uses Docker" 0 "os=linux arch=arm64 server=docker" \
  "$docker_ok" KYB_OS=Linux KYB_ARCH=aarch64
case_ "brew on Linux is not used" 0 "server=docker" \
  "$both" KYB_OS=Linux KYB_ARCH=x86_64
case_ "Docker this user cannot reach is a clear error" 1 "not usable here" \
  "$docker_down" KYB_OS=Linux KYB_ARCH=x86_64
case_ "Linux without Docker says to install Docker" 1 "Install Docker Engine" \
  "$none" KYB_OS=Linux KYB_ARCH=x86_64
case_ "Apple Silicon with neither brew nor Docker still installs natively" 0 "server=native" \
  "$none" KYB_OS=Darwin KYB_ARCH=arm64
case_ "Intel Mac without Docker says to install Docker Desktop" 1 "Install Docker Desktop" \
  "$none" KYB_OS=Darwin KYB_ARCH=x86_64
mkdir -p "$work/agents"; : > "$work/agents/kyb.plist"
case_ "a native install from this script is upgraded, even with brew present" 0 "server=native" \
  "$answering:$brew_none" KYB_OS=Darwin KYB_ARCH=arm64 KYB_PLIST="$work/agents/kyb.plist"
case_ "native can be forced over brew" 0 "server=native" \
  "$brew_only" KYB_OS=Darwin KYB_ARCH=arm64 KYB_MODE=native
case_ "a server already answering is left alone" 0 "server=existing" \
  "$answering:$docker_ok" KYB_OS=Linux KYB_ARCH=x86_64
mkdir -p "$work/prev"; : > "$work/prev/docker-compose.yml"
case_ "a running Docker install from this script is upgraded" 0 "server=docker" \
  "$answering:$docker_ok" KYB_OS=Linux KYB_ARCH=x86_64 KYB_DIR="$work/prev"
case_ "a running server not from brew on a Mac with brew is left alone" 0 "server=existing" \
  "$answering:$brew_none" KYB_OS=Darwin KYB_ARCH=arm64
case_ "an installed server points the CLI at this machine" 0 "cli->127.0.0.1" \
  "$docker_ok" KYB_OS=Linux KYB_ARCH=x86_64
case_ "client mode points the CLI at the named server" 0 "server=client version=v0.3.3 cli->kyb.example.net" \
  "$none" KYB_OS=Linux KYB_ARCH=x86_64 KYB_MODE=client KYB_SERVER=kyb.example.net
case_ "a forced mode wins over detection" 0 "server=docker" \
  "$brew_only" KYB_OS=Darwin KYB_ARCH=arm64 KYB_MODE=docker
case_ "a version without v is normalised" 0 "version=v0.3.3" \
  "$docker_ok" KYB_OS=Linux KYB_ARCH=x86_64 KYB_VERSION=0.3.3
case_ "an unknown mode is refused" 1 "KYB_MODE must be" \
  "$none" KYB_OS=Linux KYB_ARCH=x86_64 KYB_MODE=deb

# The installer fetches the compose file it documents from the site; both must exist.
[ -f "$repo_root/docs/docker-compose.yml" ] || { echo "FAIL docs/docker-compose.yml missing"; fail=1; }

[ "$fail" = 0 ] && echo "install.sh: all cases pass"
exit "$fail"

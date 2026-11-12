#!/bin/bash
# 构建 release 三二进制并打包 NSIS 安装器。
# 用法（Git Bash）：./scripts/build-installer.sh
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release -p client-core -p client-gui -p client-sidecar
mkdir -p target/installer

export MSYS2_ARG_CONV_EXCL="*"
"/c/Program Files (x86)/NSIS/makensis.exe" /V2 scripts/installer.nsi
ls -la target/installer/
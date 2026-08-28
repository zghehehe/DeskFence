#!/usr/bin/env bash
# 把 website/index.html 发布到 GitHub Pages(用户主页仓库 zghehehe.github.io)。
# 前置(一次性):在 GitHub 建一个名为 zghehehe.github.io 的 Public 空仓库,
# 不要勾选任何初始化选项。
# 用法: tools/deploy-site.sh
set -euo pipefail
cd "$(dirname "$0")/.."

SITE_DIR="$PWD/.site-deploy"
REPO="git@github.com:zghehehe/zghehehe.github.io.git"

if [ ! -d "$SITE_DIR/.git" ]; then
  git init -q -b main "$SITE_DIR"
  git -C "$SITE_DIR" remote add origin "$REPO"
fi
cp website/index.html "$SITE_DIR/index.html"
git -C "$SITE_DIR" add -A
GIT_AUTHOR_NAME=zghehehe GIT_AUTHOR_EMAIL=zghehehe@users.noreply.github.com \
GIT_COMMITTER_NAME=zghehehe GIT_COMMITTER_EMAIL=zghehehe@users.noreply.github.com \
  git -C "$SITE_DIR" commit -q -m "site update $(date +%F)" || echo "(内容无变化)"
git -C "$SITE_DIR" push -u origin main

echo "==> 已发布。1-2 分钟后 https://zghehehe.github.io 生效(首次部署需在仓库"
echo "    Settings -> Pages 确认 Source 为 deploy from branch / main / root)。"

#!/usr/bin/env bash
# DeskFence GitHub 发布脚本
# 把 master 工作区里的"公开文件集"同步成 main 分支(孤儿历史,纯发布内容)并推送。
# master 上的一切私有内容(AGENTS.md/tools/docs/website 等)永远不会进入 main。
#
# 用法(在仓库任意位置, Git Bash 执行):
#   tools/publish-to-github.sh              # 仅同步并推送 main
#   tools/publish-to-github.sh v0.1.1       # 同步推送 main + 打 tag 并推送(触发 Actions 自动发 Release)
#
# 首次使用前(一次性):
#   1) GitHub 建空仓库(不勾任何初始化选项)
#   2) git remote add public git@github.com:zghehehe/DeskFence.git
# 双仓架构(2026-09-11): origin=私有仓 DeskFence-private(永不公开,日常开发);
# public=对外发布仓 DeskFence(只有 main + v* tag;master 绝不推到 public)。
set -euo pipefail
cd "$(dirname "$0")/.."

TAG="${1:-}"
REMOTE="public"

git remote get-url "$REMOTE" >/dev/null 2>&1 || {
  echo "!! 还没有配置 $REMOTE。先执行:"
  echo "   git remote add public git@github.com:zghehehe/DeskFence.git"
  exit 1
}
if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "!! 工作树有未提交改动,先 git add + git commit 再发布"
  exit 1
fi

# 公开提交一律用中性身份(不用工作邮箱)
export GIT_AUTHOR_NAME=zghehehe
export GIT_AUTHOR_EMAIL=zghehehe@users.noreply.github.com
export GIT_COMMITTER_NAME=zghehehe
export GIT_COMMITTER_EMAIL=zghehehe@users.noreply.github.com

export GIT_INDEX_FILE="$PWD/.git/pub-idx"
trap 'unset GIT_INDEX_FILE; rm -f .git/pub-idx' EXIT
git read-tree --empty
git add .cargo .github src resources assets/deskfence.ico assets/deskfence-icon.svg \
        docs/demo.svg \
        Cargo.toml Cargo.lock build.rs DeskFence.rc app.manifest \
        README.md LICENSE .gitignore
T=$(git write-tree)
P=""
git rev-parse --verify main >/dev/null 2>&1 && P="-p main"
C=$(git commit-tree $T $P -m "sync from master $(git rev-parse --short HEAD)")
git branch -f main "$C"
trap - EXIT
unset GIT_INDEX_FILE
rm -f .git/pub-idx

echo "==> main 已更新: $(git rev-parse --short main)"
echo "==> 推送 main ..."
git push -u public main

if [ -n "$TAG" ]; then
  git tag -f "$TAG" main
  echo "==> 推送 tag $TAG (Actions 将自动构建并发布 Release) ..."
  git push public "$TAG"
  echo "==> 完成。进度看 GitHub 仓库的 Actions 页;产物在 Releases 页。"
else
  echo "==> 完成(未发版)。要发版请带版本号重跑, 如: tools/publish-to-github.sh v0.1.1"
fi

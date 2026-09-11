<div align="center">

<img src="assets/deskfence-icon.svg" width="88" alt="DeskFence 图标">

# DeskFence

**把 Windows 桌面还给你 —— 自动分类、原生观感、零感知的桌面图标栅栏**

[![Release](https://img.shields.io/github/v/release/zghehehe/DeskFence?label=%E6%9C%80%E6%96%B0%E7%89%88%E6%9C%AC)](https://github.com/zghehehe/DeskFence/releases/latest)
[![Stars](https://img.shields.io/github/stars/zghehehe/DeskFence?style=flat&logo=github&label=Stars)](https://github.com/zghehehe/DeskFence/stargazers)
[![License](https://img.shields.io/badge/license-GPL--3.0%20%2B%20Commercial-blue)](LICENSE)
![Platform](https://img.shields.io/badge/platform-Windows%2010%20%7C%2011-blue)
![Rust](https://img.shields.io/badge/Rust-%F0%9F%A6%80-orange)

[**⬇ 下载最新版**](https://github.com/zghehehe/DeskFence/releases/latest/download/deskfence.exe) · [全部版本](https://github.com/zghehehe/DeskFence/releases) · [问题反馈](https://github.com/zghehehe/DeskFence/issues)

<img src="docs/demo.svg" width="760" alt="启动 DeskFence,桌面图标自动归类入栅栏(演示动画)">

*单文件 · 免安装 · 静态链接,无需任何运行库*

</div>

---

轻量级 Windows 桌面图标栅栏整理工具。把桌面图标按类别收进"栅栏"，图标、
图标名、右键菜单全部取自 Windows Shell 原生资源，观感与原生桌面一致。
Rust + Win32 + Direct2D 实现。

## 特性

- **分类双模式**：
  - 自动分类：软件 / 文件夹 / 文档 / 图片 / 媒体 / 代码 / 压缩包 / 其他
    自动分栏，新文件落下自动入位
  - 自定义（拖入归类）：文件只进被拖入的栅栏，分类完全由你决定
  - 分类管理面板（托盘 → 自动分类 ▸）：分类改名、删除、新增，每类可
    编辑扩展名规则，保存后全部文件归属即时重算
  - 支持手动钉选、拖入拖出
- **每栅栏独立排序**：常用（按使用频率）/ 时间（最近修改）/ 名称 /
  手动（拖拽自定义）
- **原生观感**：图标取自系统图像列表（含 .lnk 箭头），图标名用 Explorer
  同源字体 + ClearType 渲染；精确模式下与原生桌面逐像素一致，另有
  透明模式兜底特殊环境
- **对齐与拖拽三档**：自动对齐（固定间隔）/ 网格对齐（图标格倍数）/
  自由移动（邻居磁吸）；栅栏与图标拖动时插入线实时提示落位，
  其余内容不动，松手才拼接，Esc 取消
- **三态桌面**（托盘菜单切换，重启保持）：
  - 正常：栅栏接管桌面
  - 纯净：栅栏与图标都隐藏，只剩壁纸
  - 原生：恢复 Explorer 原生图标
- **边框线随心显隐**：默认显示细边框便于观察分组；关掉后平时完全
  干净，悬停才浮现边框/标题/手柄（托盘一键切换）
- **栅栏操作**：新建 / 重命名 / 折叠 / 锁定 / 删除；内容多时滚轮滚动，
  框选批量移动，图标就地重命名（与 Explorer 同款）
- **中英双语**：托盘 → 语言 / Language 即时切换（跟随系统/中文/English），
  无需重启；界面文字与原生菜单完全一致
- **首启引导（一次性）**：首次启动弹出说明窗——桌面已被归类、如何一键
  还原，并默认勾选"显示栅栏边框线"与"开机自启"（不需要取消勾选即可），
  关闭后绝不再弹
- **多显示器 + 高 DPI**：跨屏 DPI 变更即时自适应，跟随系统 Ctrl+滚轮的
  图标大小；开机自启动（托盘开关），进程拉起后约 0.3 秒全量呈现
- 布局可反悔：撤销上次布局调整、一键恢复默认布局；Explorer 重启自愈
- **双击即恢复**：任何时候双击 exe 都得到一个健康的 DeskFence——旧实例
  先优雅退出（恢复图标、保存状态）让位，新实例接管；旧实例卡死收不到
  退出消息，2 秒后自动强杀兜底。升级新版也是同样双击即可

## 安装

### 方式一：直接下载（推荐）

1. 到 [Releases](https://github.com/zghehehe/DeskFence/releases/latest) 下载 `deskfence.exe`
2. 双击运行即可。首次运行如遇 SmartScreen 提示：点"更多信息 → 仍要运行"
3. 程序常驻托盘（右下角图标），右键托盘图标进行管理

卸载 = 托盘退出后删除 exe 与 `%APPDATA%\DeskFence` 目录；若勾选过
开机自启，可顺手在任务管理器"启动应用"里移除残留项（不删也无害）。

### 方式二：源码构建

前置：[Rust](https://rustup.rs) stable（MSVC 工具链，需安装
[Visual Studio 生成工具](https://visualstudio.microsoft.com/zh-hans/downloads/)）。

```bash
git clone https://github.com/zghehehe/DeskFence.git
cd DeskFence
cargo build --release
# 产物：target/release/deskfence.exe
```

应用图标与清单资源已预编译内置（`resources/deskfence.res`），构建不需要
windres 等额外工具。

## 使用速览

| 操作 | 方式 |
|---|---|
| 新建栅栏 / 显隐栅栏 / 对齐方式 / 渲染模式 | 桌面空白右键 → DeskFence 子菜单 |
| 分类管理（改名 / 删除 / 新增 / 扩展名规则） | 右键托盘图标 → 自动分类 ▸ |
| 栅栏排序（常用/时间/名称/手动） | 栅栏标题倒三角 ▾ → 排序方式 |
| 纯净桌面 / 恢复原生桌面 / 撤销 / 恢复默认布局 / 开机自启 | 右键托盘图标 |
| 移动/缩放/折叠/锁定栅栏 | 拖标题栏、拖角手柄、栅栏右键菜单 |
| 拖拽取消 | 按住时按 Esc |
| **自救**：卡死 / 桌面异常 | 再次双击 `deskfence.exe`（旧实例自动让位，卡死 2 秒强杀） |
| 仅恢复原生桌面图标（无 UI） | `deskfence.exe --restore-desktop`（同样先清退旧实例） |

数据位置：`%APPDATA%\DeskFence`（配置、壁纸与图标缓存），删除即重置。
程序不联网、不收集任何数据——完全开源，可自行审计。

## 常见问题

- **分类能自定义吗？** 托盘 → 自动分类 ▸ 打开分类管理面板：可改名、
  删除、新增分类，并为每个分类编辑扩展名规则；也可切到
  "自定义（拖入归类）"模式，文件只进你拖入的栅栏。
- **SmartScreen / 杀软提示？** 程序未做付费代码签名。完全开源可自行审阅
  与源码构建；杀软偶发静态链接误报，加白名单即可。
- **出问题桌面失控？** 再次双击 `deskfence.exe` 即可——旧实例优雅退出、
  卡死 2 秒强杀，重启后的实例一定是健康的。仅需无 UI 恢复图标时运行
  `deskfence.exe --restore-desktop`（同样会先清退旧实例）。

## License

采用 **GPL-3.0 + 商业授权** 双许可（详见 [LICENSE](LICENSE)）：

- **开源使用**：个人使用、学习、修改、再分发遵循
  [GNU GPL-3.0](https://www.gnu.org/licenses/gpl-3.0.html)——含商用，
  但衍生物须同样以 GPL-3.0 开源
- **商业授权**：闭源集成、OEM 定制等无法满足 GPL 义务的场景，可经
  [GitHub Issues](https://github.com/zghehehe/DeskFence/issues) 与作者
  协商单独授权

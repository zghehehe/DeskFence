# DeskFence

轻量级桌面图标整理栅栏工具（Rust + Win32 + Direct2D）。把桌面图标按类型收进
透明栅栏，图标本体、图标名、右键菜单均取自 Windows Shell 原生资源，观感与
原生桌面一致。

## 功能

- 按类别（图片/文件夹/软件/其他…）自动分栏，支持手动钉选、拖入、拖出
- 图标与图标名取自系统图像列表和 Explorer 桌面字体（含 GDI 字符格换算）
- 双渲染模式（桌面右键 → DeskFence 子菜单切换）：
  - 透明（默认）：兼容动态壁纸（Wallpaper Engine 等）
  - 精确：不透明壁纸底 + GDI ClearType 图标名，静态壁纸/幻灯片下与原生
    桌面逐像素一致；检测到动态壁纸自动降级回透明
- 栅栏拖拽为插入式：拖动中该栅栏跟手置顶、其余不动，插入点以指示线
  提示，松手才拼接重排，Esc 取消
- 栅栏内图标拖拽同款：被拖图标残影跟手（观感同原生拖拽图像）、目标槽
  与插入线实时提示，松手按块拼接落位
- 栅栏可拖动/缩放/折叠/锁定，三档对齐（自动 / 网格 / 自由）
- 原生桌面右键菜单（DefView 同源），文件级右键菜单走真实 Shell IContextMenu
- 撤销、刷新、单实例、开机自启、多显示器与高 DPI

## 构建

```
cargo build --release
```

产物：`target/release/deskfence.exe`

## 命令行

| 参数 | 作用 |
|---|---|
| （无） | 常驻运行 |
| `--restore-desktop` | 恢复 Explorer 原生桌面图标（自救模式，不加载任何 UI） |
| `--icondump <file> <out-prefix>` | 诊断：导出图标提取路径的像素，供与原生截图对比 |

## 代码结构

| 文件 | 职责 |
|---|---|
| `src/main.rs` | 入口、单实例、恢复模式 |
| `src/ui.rs` | 窗口、命中测试、拖动/缩放/滚动、菜单、托盘 |
| `src/model.rs` | 布局与对齐算法、配置持久化、使用频率统计（含单元测试） |
| `src/render.rs` | Direct2D/DirectWrite 渲染：图标、标签、滚动条 |
| `src/shell.rs` | Shell 集成：图标提取、原生菜单、桌面窗口定位、自启 |
| `src/ole.rs` | 拖放（DragDrop 出/入） |
| `assets/` | 应用图标（矢量源 `deskfence-icon.svg` + 打包好的 `deskfence.ico`） |

图标改版流程：编辑 SVG → `python tools/rebuild_icon.py` → `cargo build --release`。

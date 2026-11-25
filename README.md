# ZettelQuick

专为 Windows 与 **Zettelkasten（卡片盒笔记法）** 设计的零摩擦全局速记与双链捕获工具，100% 兼容 **Obsidian** 本地 Markdown 知识库规范。

## 特性亮点

- **绿色单文件便携**：编译产物为单个免安装 `zettel-quick.exe`，配置集中于同级 `config.toml`（修改保存自动热重载），Edge WebView2 运行缓存隔离于 `%LOCALAPPDATA%\ZettelQuick`，不污染便携目录。
- **瞬时呼出与多屏跟随**：后台静默驻留系统托盘，按下全局快捷键自动在当前鼠标所在显示器居中偏上弹出无边框圆角卡片窗（支持 Windows 11 原生圆角阴影与置顶 Pin）。
- **纯粹输入与中文 IME 友好**：100% 纯文本编辑体验，内置光标像素级跟随的 `[[双链]]` 与 `#标签` 全库增量模糊补全。
- **零阻塞自动命名与元数据**：打开即写，按 `Esc` 或 `Ctrl+Enter` 自动提取首行摘要拼接时间戳落盘，并同步生成 `id` / `created` / `tags` / `aliases` YAML Frontmatter。
- **防误触与追改保障**：支持 `Alt+↑` 一键唤回刚归档的上一张卡片原地更新（自动同步重命名）、粘贴截图自动存入附件夹并插入 `![[...png]]`、以及未归档草稿防崩溃恢复。

## 快捷键速查

| 快捷键 | 功能说明 |
| :--- | :--- |
| `Ctrl+Alt+N` *(默认全局)* | 唤起置前卡片窗；已聚焦时再次按下则临时隐藏并保留草稿 |
| `Esc` | 若补全菜单打开则关闭补全；否则自动保存归档为 `.md` 并隐藏 |
| `Ctrl+Enter` / `Ctrl+S` | 立即保存归档为 `.md` 并隐藏 |
| `Ctrl+Shift+Enter` | 连续制卡：保存当前卡片并清空编辑器继续写下一张 |
| `Alt+Up` | 草稿为空时，唤回最近保存的一张卡片进行原地修订 |
| `Ctrl+Tab` | 在预设的目标落盘目录（如 `00-Inbox` / `10-Permanent`）间循环切换 |
| `Ctrl+Shift+P` | 取消最外层固定并将笔记窗口放到其他窗口后面（再次按 `Ctrl+Alt+N` 或 `Ctrl+Shift+P` 恢复最外层置顶） |
| `Ctrl+Shift+Backspace` | 主动放弃并清空当前未保存草稿（不生成文件） |
| `Enter` / `Shift+Enter` *(补全列表)* | `Enter` 插入 `[[文件名]]`；`Shift+Enter` 插入 `[[文件名\|可读标题]]` |

## 运行日志与故障排查

程序内置按大小自动轮转的全链路行为与异常诊断日志（不污染 `.exe` 所在的便携目录，且不会在普通级别记录私人笔记正文内容）：

- **日志默认路径**：`%LOCALAPPDATA%\ZettelQuick\logs\zettel-quick.log`（超过 `5 MB` 自动轮转为 `.1.log` ~ `.3.log`）。
- **快捷查看方式**：右键右下角系统托盘图标 → 点击 **「查看运行日志 (zettel-quick.log)」** 即可直接打开最新日志。
- **动态调整详细程度**：在 `config.toml` 的 `[logging]` 段落中将 `level = "info"` 改为 `level = "debug"` 并保存，无需重启即可热重载输出细粒度调试事件（包含多显示器像素坐标、文件监听增量变更、草稿防抖同步等）。
- **覆盖范围**：进程启动参数、配置解析与热重载语法错误行号、全局热键占用冲突、多屏焦点切换、卡片归档/回改耗时、截图编码落盘、全库索引耗时、前端未捕获异常及 Rust Panic 崩溃堆栈。

## 开发与构建

```powershell
# 安装前端构建依赖
npm install

# 运行后端单元测试
cargo test --manifest-path src-tauri/Cargo.toml

# 构建独立单文件绿色版 (.exe 输出至 src-tauri/target/release/zettel-quick.exe)
npx tauri build --no-bundle
```

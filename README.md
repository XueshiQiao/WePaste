# WePaste

**无需切换电脑输入法，直接双向同步手机「微信输入法」与电脑（macOS / Windows）系统剪贴板。**

手机上用微信输入法很顺手，但在电脑（Mac / Windows）上往往主力使用系统自带输入法、Rime 或搜狗输入法。官方微信输入法桌面端要求切换到它的输入法状态下才能完整使用跨设备剪贴板。

**WePaste** 基于开源的微信输入法跨设备通信协议实现，是一个常驻菜单栏（macOS Menu Bar）/ 系统托盘（Windows System Tray）的轻量桌面小工具：
- **手机复制 ──▶ 电脑直接 `⌘V` / `Ctrl+V`**：后台每 2 秒自动监听手机微信输入法复制的**文字与图片（截图/相册照片）**，直接写入电脑系统剪贴板，完全不绑定电脑当前使用什么输入法。
- **电脑剪贴板 ──▶ 一键发给手机**：随时将电脑当前剪贴板中的文字或截图推送到手机微信输入法候选栏。
- **Arc Split-Deck 流光双太空舱界面**：内置「☀️ 亮色版」与「🌙 暗色版」双主题界面，支持窗口任意空白处拖拽、主显示设备自由切换、开机自动启动与 GitHub Releases 自动检查更新。

---

## 核心特性

- **零输入法侵入**：不需要在 Mac / Windows 上安装或切换到微信输入法，保持原有的打字习惯。
- **文字 + 图片双向互通**：
  - 自动接收手机复制的文字、网页链接、手机截图，解码后直接写入电脑系统剪贴板并居中展示缩略图。
  - 支持读取电脑当前剪贴板的文字或截图，一键编码加密上传至手机端微信输入法。
- **6 位数字码直连配对**：打开手机「微信输入法 App → 跨设备复制粘贴」，在 WePaste 输入手机显示的 6 位数字配对码并在手机点确认即可完成绑定。
- **跨平台原生常驻**：
  - **macOS**：`ActivationPolicy::Accessory` 菜单栏常驻，原生支持 Apple Silicon (`aarch64`) 与 Intel (`x86_64`)。
  - **Windows**：系统托盘常驻，原生支持 `x64` 与 `ARM64`。
- **自动更新与开机自启**：内置 `tauri-plugin-updater` 与 `tauri-plugin-autostart`，支持菜单栏右键菜单和设置面板内一键检查并安装新版本。

---

## 目录结构

```text
WePaste/
├── Cargo.toml                 # Workspace 根配置
├── ui/
│   └── index.html             # Arc Split-Deck 亮/暗双主题前端面板
├── src-tauri/                 # Tauri v2 桌面端宿主（系统剪贴板读写、托盘、自动更新、开机自启）
│   ├── src/main.rs
│   ├── tauri.conf.json
│   └── entitlements.plist
├── crates/
│   ├── wx-ime-protocol/       # 微信输入法底层加密与协议原语（ECDH P-256 / HKDF / AES-GCM / Protobuf）
│   └── wx-ime-sdk/            # 跨设备注册、6 位码配对、群组管理、COS 大文件加解密与剪贴板收发客户端
└── .github/workflows/
    ├── ci.yml                 # macOS + Windows 自动编译与单元测试
    └── release.yml            # 多架构安装包构建、自动更新签名与 latest.json 发布流水线
```

---

## 本地开发与构建

### 环境依赖

1. **Rust 工具链**（`>= 1.85`）
2. **Protobuf 编译器 (`protoc`)**
   - macOS：`brew install protobuf`
   - Windows：`choco install protoc` 或 `winget install Google.Protobuf`

### 编译与测试

```bash
# 运行协议与 SDK 单元测试
cargo test -p wx-ime-protocol -p wx-ime-sdk

# 编译桌面端（macOS 使用 Homebrew OpenSSL）
OPENSSL_DIR=/opt/homebrew/opt/openssl@3 cargo build

# 跨平台免外部 OpenSSL 依赖编译（内置静态编译 OpenSSL）
cargo build --features wepaste/vendored-openssl
```

---

## 致谢（Credits）

WePaste 的诞生离不开以下优秀开源项目的探索与基础支持，特此致谢：

- **[mkdir700/wx-ime-sdk](https://github.com/mkdir700/wx-ime-sdk)**（作者：[@mkdir700](https://github.com/mkdir700)，`MIT OR Apache-2.0`）：微信输入法跨设备通信协议（`wx-ime-protocol` 与 `wx-ime-sdk`）的完整逆向分析与 Rust 实现，涵盖了 ECDH P-256 密钥协商、AES-GCM 报文加解密、6 位配对码握手以及腾讯云 COS 大文件/图片分片传输，是本项目最核心的协议基石。
- **[mkdir700/wetype-clipboard-mcp](https://github.com/mkdir700/wetype-clipboard-mcp)**（作者：[@mkdir700](https://github.com/mkdir700)）：基于微信输入法跨设备剪贴板协议实现的 MCP 工具服务，为本项目的状态机设计与剪贴板收发工作流提供了重要参考。
- **[tauri-apps/tauri](https://github.com/tauri-apps/tauri)**（`Apache-2.0 OR MIT`）：提供跨平台（macOS / Windows）轻量级桌面窗口、系统托盘（Tray Icon）、开机自启（`tauri-plugin-autostart`）、单实例守护（`tauri-plugin-single-instance`）与自动升级（`tauri-plugin-updater`）能力。
- **[1Password/arboard](https://github.com/1Password/arboard)**（`MIT OR Apache-2.0`）：1Password 开源的跨平台系统剪贴板读写库，用于将手机同步过来的文字与 RGBA 图像直接写入 macOS 与 Windows 系统剪贴板。
- **[image-rs/image](https://github.com/image-rs/image)**（`MIT OR Apache-2.0`）：用于在系统剪贴板原始像素与 PNG 图像格式之间进行高效编解码。

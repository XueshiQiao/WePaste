# WePaste 本地开发、目录结构与发布指南

本文档面向参与 **WePaste** 本地编译、调试或配置 GitHub Actions 自动化发布的开发者。

---

## 目录结构

```text
WePaste/
├── Cargo.toml                 # Workspace 根配置
├── package.json               # Tauri CLI 构建入口
├── assets/
│   └── workflow.svg           # README 使用场景与双向同步示意图
├── ui/
│   └── index.html             # Arc Split-Deck 亮/暗双主题前端面板（支持浏览器独立预览）
├── src-tauri/                 # Tauri v2 桌面端宿主（系统剪贴板读写、托盘、自动更新、开机自启）
│   ├── src/main.rs            # Rust 宿主主程序
│   ├── tauri.conf.json        # Tauri 窗口、打包与 Updater 公钥配置
│   ├── capabilities/          # Tauri v2 权限声明（autostart / updater / core）
│   ├── icons/                 # macOS (.icns) / Windows (.ico) / PNG 多尺寸图标
│   └── entitlements.plist     # macOS 开发者签名 Entitlements
├── crates/
│   ├── wx-ime-protocol/       # 微信输入法底层加密与协议原语（ECDH P-256 / HKDF / AES-GCM / Protobuf）
│   └── wx-ime-sdk/            # 跨设备注册、6 位码配对、群组管理、COS 大文件加解密与剪贴板收发客户端
└── .github/workflows/
    ├── ci.yml                 # macOS + Windows 自动编译与单元测试
    └── release.yml            # 多架构安装包构建、自动更新签名与 latest.json 发布流水线
```

---

## 本地开发与构建

### 1. 环境依赖

1. **Rust 工具链**（`>= 1.85`）
2. **Protobuf 编译器 (`protoc`)**（用于编译 `crates/wx-ime-protocol/proto/*.proto`）
   - macOS：`brew install protobuf`
   - Windows：`choco install protoc` 或 `winget install Google.Protobuf`
3. **Node.js**（仅在使用 `npx tauri build` 打包安装包时需要）

### 2. 编译与单元测试

```bash
# 运行底层协议与 SDK 单元测试
cargo test -p wx-ime-protocol -p wx-ime-sdk

# 本地开发编译（macOS 使用 Homebrew OpenSSL）
OPENSSL_DIR=/opt/homebrew/opt/openssl@3 cargo build

# 跨平台免外部 OpenSSL 依赖编译（通过 openssl-src 静态编译内置 OpenSSL）
cargo build --features wepaste/vendored-openssl
```

### 3. UI 面板浏览器独立预览

`ui/index.html` 内置了浏览器预览模式（包含亮色/暗色同屏对比与「正常收发 / 收到手机截图 / 首次配对输码」三种预设数据状态），无需启动 Rust 后端即可直接在浏览器打开调试：

```bash
open ui/index.html
```

### 4. macOS 本地开发者签名

为避免 macOS 每次重编译后丢失系统权限，本地打包 `.app` 时请使用有效的 Apple Development 证书签名：

```bash
codesign --force --deep --sign "Apple Development: xueshi qiao (AB98XH368Q)" \
  --entitlements src-tauri/entitlements.plist \
  target/debug/bundle/macos/WePaste.app
```

---

## GitHub Actions 自动构建与自动升级签名

仓库内置了 `.github/workflows/ci.yml` 与 `.github/workflows/release.yml`：
- 推送 `v*` 标签（如 `git tag v0.1.0 && git push origin v0.1.0`）或在 GitHub Actions 页面手动触发 `Build & Release`，即可自动构建四端产物并生成 `latest.json`：
  - `WePaste_<ver>_macos_aarch64.dmg` + `WePaste_macos_aarch64.app.tar.gz`
  - `WePaste_<ver>_macos_x64.dmg` + `WePaste_macos_x64.app.tar.gz`
  - `WePaste_<ver>_windows_x64-setup.exe`
  - `WePaste_<ver>_windows_arm64-setup.exe`
  - `latest.json`（供客户端 `tauri-plugin-updater` 自动检测升级）

### Actions Secrets 说明

| Secret 名称 | 用途 | 是否必填 |
|---|---|---|
| `TAURI_SIGNING_PRIVATE_KEY` | Tauri 自动更新包签名私钥（对应 `src-tauri/tauri.conf.json` 中的 `pubkey`） | 开启自动升级必填 |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | 私钥密码（若生成免密私钥则无需配置） | 可选 |
| `APPLE_CERTIFICATE` | macOS Developer ID 证书（`.p12` 的 Base64 编码） | 可选（用于公证分发） |
| `APPLE_CERTIFICATE_PASSWORD` | `.p12` 证书导出密码 | 可选 |
| `APPLE_SIGNING_IDENTITY` | 签名证书名称（如 `Developer ID Application: ...`） | 可选 |
| `APPLE_ID` / `APPLE_PASSWORD` / `APPLE_TEAM_ID` | Apple 公证（Notarization）凭据 | 可选 |

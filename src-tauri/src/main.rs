#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use arboard::{Clipboard, ImageData};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chrono::Local;
use image::{DynamicImage, ImageFormat};
use serde::Serialize;
use std::{
    borrow::Cow,
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tauri::{
    AppHandle, Manager, State, WindowEvent,
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt as AutostartExt};
use tauri_plugin_updater::UpdaterExt;
use tokio::sync::Mutex;
use wx_ime_sdk::{Client, ClientOptions, ClipboardContent, PairingCode, ReceivedClipboard};

#[derive(Debug, Clone, Serialize)]
pub struct DeviceDto {
    pub name: String,
    pub uin: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppStateSnapshot {
    pub platform: String,
    pub app_version: String,
    pub paired: bool,
    pub auto_sync: bool,
    pub autostart: bool,
    pub update_available: Option<String>,
    pub local_name: String,
    pub local_uin: String,
    pub last_preview: String,
    pub last_image_data_url: Option<String>,
    pub last_kind: String,
    pub last_source: String,
    pub last_time: String,
    pub local_clipboard_preview: String,
    pub local_clipboard_image_data_url: Option<String>,
    pub local_clipboard_kind: String,
    pub banner: Option<String>,
    pub banner_type: String,
    pub devices: Vec<DeviceDto>,
}

struct CachedLocalImage {
    hash: blake3::Hash,
    width: usize,
    height: usize,
    data_url: String,
}

struct SharedData {
    client: Option<Client>,
    state_dir: PathBuf,
    device_name: String,
    snapshot: AppStateSnapshot,
    local_img_cache: Option<CachedLocalImage>,
}

#[derive(Clone)]
struct AppContext {
    auto_sync: Arc<AtomicBool>,
    inner: Arc<Mutex<SharedData>>,
    sync_check_item: Arc<std::sync::Mutex<Option<CheckMenuItem<tauri::Wry>>>>,
    autostart_check_item: Arc<std::sync::Mutex<Option<CheckMenuItem<tauri::Wry>>>>,
}

fn resolve_state_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|h| h.join("Library/Application Support/me.xueshi.wepaste"));

    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .map(|d| d.join("me.xueshi.wepaste"));

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|h| h.join(".config/wepaste"));

    let dir = base.unwrap_or_else(|| PathBuf::from(".wepaste-state"));
    let _ = fs::create_dir_all(&dir);

    // 若新目录还没有 identity.json，但 ~/.config/wxc 下已有配对好的身份，顺手迁移过来以便开箱即用
    let target_identity = dir.join("identity.json");
    if !target_identity.exists()
        && let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
    {
        let legacy_dir = home.join(".config/wxc");
        let legacy_identity = legacy_dir.join("identity.json");
        if legacy_identity.exists() {
            let _ = fs::copy(&legacy_identity, &target_identity);
            let legacy_name = legacy_dir.join("device-name");
            if legacy_name.exists() {
                let _ = fs::copy(&legacy_name, dir.join("device-name"));
            }
        }
    }
    dir
}

fn detect_system_host_name() -> String {
    #[cfg(target_os = "windows")]
    if let Some(name) = std::env::var_os("COMPUTERNAME")
        .or_else(|| std::env::var_os("HOSTNAME"))
        .and_then(|s| s.into_string().ok())
    {
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            return trimmed.to_owned();
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = std::process::Command::new("scutil")
            .args(["--get", "ComputerName"])
            .output()
            && out.status.success()
            && let Ok(s) = String::from_utf8(out.stdout)
        {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return trimmed.to_owned();
            }
        }
        if let Ok(out) = std::process::Command::new("hostname").arg("-s").output()
            && out.status.success()
            && let Ok(s) = String::from_utf8(out.stdout)
        {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return trimmed.to_owned();
            }
        }
    }

    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let hash_hex = blake3::hash(&seed.to_le_bytes()).to_hex();
    let suffix = &hash_hex.as_str()[..4].to_uppercase();
    if cfg!(target_os = "windows") {
        format!("DESKTOP-{suffix}")
    } else {
        format!("MAC-{suffix}")
    }
}

fn resolve_device_name(state_dir: &Path) -> String {
    let name_file = state_dir.join("device-name");
    let identity_file = state_dir.join("identity.json");

    if let Ok(existing) = fs::read_to_string(&name_file) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() && !trimmed.eq_ignore_ascii_case("WePaste") {
            return trimmed.to_owned();
        }
    }

    if let Ok(raw) = fs::read_to_string(&identity_file)
        && let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw)
        && let Some(name) = val.get("device_name").and_then(|v| v.as_str())
    {
        let trimmed = name.trim();
        if !trimmed.is_empty() && !trimmed.eq_ignore_ascii_case("WePaste") {
            let _ = fs::write(&name_file, trimmed);
            return trimmed.to_owned();
        }
    }

    let host_name = detect_system_host_name();
    let _ = fs::write(&name_file, &host_name);

    // 如果旧 identity.json 里存的是 "WePaste"，一并原地更新为系统主机名，避免重新生成身份
    if let Ok(raw) = fs::read_to_string(&identity_file)
        && let Ok(mut val) = serde_json::from_str::<serde_json::Value>(&raw)
        && let Some(obj) = val.as_object_mut()
    {
        obj.insert(
            "device_name".to_owned(),
            serde_json::Value::String(host_name.clone()),
        );
        if let Ok(updated) = serde_json::to_string_pretty(&val) {
            let _ = fs::write(&identity_file, updated);
        }
    }

    host_name
}

fn encode_thumbnail_data_url(img: &DynamicImage) -> Option<String> {
    let thumb = img.thumbnail(640, 360);
    let mut buf = Cursor::new(Vec::new());
    thumb.write_to(&mut buf, ImageFormat::Png).ok()?;
    let b64 = BASE64.encode(buf.into_inner());
    Some(format!("data:image/png;base64,{b64}"))
}

fn write_received_to_os_clipboard(
    received: &ReceivedClipboard,
) -> Result<(String, String, Option<String>), String> {
    let mut clipboard = Clipboard::new().map_err(|e| format!("无法打开系统剪贴板: {e}"))?;
    match &received.content {
        ClipboardContent::Text(text) => {
            clipboard
                .set_text(text.clone())
                .map_err(|e| format!("写入文字到系统剪贴板失败: {e}"))?;
            let char_count = text.chars().count();
            let preview: String = text.chars().take(160).collect();
            Ok((preview, format!("文字 ({char_count} 字)"), None))
        }
        ClipboardContent::Image(image_meta) => {
            let bytes = fs::read(&image_meta.path)
                .map_err(|e| format!("读取解密图片缓存失败: {e}"))?;
            let decoded = image::load_from_memory(&bytes)
                .map_err(|e| format!("解码图片格式失败: {e}"))?;
            let thumb_data_url = encode_thumbnail_data_url(&decoded);
            let rgba = decoded.to_rgba8();
            let (width, height) = rgba.dimensions();
            let img_data = ImageData {
                width: width as usize,
                height: height as usize,
                bytes: Cow::Borrowed(rgba.as_raw()),
            };
            clipboard
                .set_image(img_data)
                .map_err(|e| format!("写入图片到系统剪贴板失败: {e}"))?;
            let kb = (bytes.len() + 1023) / 1024;
            Ok((
                format!("{width}×{height} ({kb} KB)"),
                format!("图片 · {width}×{height}"),
                thumb_data_url,
            ))
        }
    }
}

fn save_preferred_peer(state_dir: &Path, peer_name: &str) {
    let trimmed = peer_name.trim();
    if !trimmed.is_empty() {
        let _ = fs::write(state_dir.join("preferred-peer"), trimmed);
    }
}

fn load_preferred_peer(state_dir: &Path) -> String {
    if let Ok(raw) = fs::read_to_string(state_dir.join("preferred-peer")) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return trimmed.to_owned();
        }
    }
    "vivo V2505A".to_owned()
}

fn device_sort_priority(d: &wx_ime_sdk::Device, preferred_name: &str) -> u8 {
    if !preferred_name.is_empty() && d.name == preferred_name {
        return 0;
    }
    let lower = d.name.to_lowercase();
    let looks_like_desktop = d.platform == 1
        || lower.contains("macbook")
        || lower.contains("mac mini")
        || lower.contains("imac")
        || lower.contains("windows")
        || lower.starts_with("wxc-");
    if !looks_like_desktop {
        1
    } else {
        2
    }
}

async fn refresh_devices_and_identity(
    client: &Client,
    state_dir: &Path,
    snapshot: &mut AppStateSnapshot,
) {
    if let Ok(local) = client.local_device().await {
        snapshot.local_name = local.name;
        snapshot.local_uin = local.uin.to_string();
    }
    let preferred = load_preferred_peer(state_dir);
    match client.group().await {
        Ok(Some(group)) => {
            snapshot.paired = true;
            let mut filtered: Vec<wx_ime_sdk::Device> = group
                .devices
                .into_iter()
                .filter(|d| d.uin.to_string() != snapshot.local_uin)
                .collect();
            filtered.sort_by_key(|d| device_sort_priority(d, &preferred));
            snapshot.devices = filtered
                .into_iter()
                .map(|d| DeviceDto {
                    name: d.name,
                    uin: d.uin.to_string(),
                })
                .collect();
        }
        Ok(None) => {
            snapshot.paired = false;
            snapshot.devices.clear();
        }
        Err(_) => {}
    }
}

async fn poll_and_commit_once(ctx: &AppContext) -> Result<bool, String> {
    let client_opt = {
        let guard = ctx.inner.lock().await;
        guard.client.clone()
    };
    let Some(client) = client_opt else {
        return Err("客户端尚未初始化".into());
    };

    match client.poll_clipboard_once().await {
        Ok(Some(received)) => {
            let (preview, kind_desc, img_data_url) = write_received_to_os_clipboard(&received)?;
            client
                .acknowledge(received.id)
                .await
                .map_err(|e| format!("确认剪贴板消费游标失败: {e}"))?;
            let now_str = Local::now().format("%H:%M:%S").to_string();
            let mut guard = ctx.inner.lock().await;
            guard.snapshot.last_preview = preview;
            guard.snapshot.last_image_data_url = img_data_url;
            guard.snapshot.last_kind = kind_desc;
            guard.snapshot.last_source = received.source.name;
            guard.snapshot.last_time = now_str;
            guard.snapshot.banner = None;
            Ok(true)
        }
        Ok(None) => Ok(false),
        Err(wx_ime_sdk::Error::NotPaired) => {
            let mut guard = ctx.inner.lock().await;
            guard.snapshot.paired = false;
            Ok(false)
        }
        Err(err) => {
            let _ = client.reconnect().await;
            Err(format!("{err}"))
        }
    }
}

fn refresh_local_os_clipboard(shared: &mut SharedData) {
    let Ok(mut clipboard) = Clipboard::new() else {
        shared.snapshot.local_clipboard_preview = String::new();
        shared.snapshot.local_clipboard_image_data_url = None;
        shared.snapshot.local_clipboard_kind = "无法读取".into();
        return;
    };
    if let Ok(text) = clipboard.get_text() {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            let count = text.chars().count();
            let preview: String = text.chars().take(180).collect();
            shared.snapshot.local_clipboard_preview = preview;
            shared.snapshot.local_clipboard_image_data_url = None;
            shared.snapshot.local_clipboard_kind = format!("文字 · {count} 字");
            return;
        }
    }
    if let Ok(img) = clipboard.get_image() {
        let hash = blake3::hash(&img.bytes);
        let data_url = if let Some(cached) = &shared.local_img_cache
            && cached.hash == hash
            && cached.width == img.width
            && cached.height == img.height
        {
            Some(cached.data_url.clone())
        } else if let Some(rgba) =
            image::RgbaImage::from_raw(img.width as u32, img.height as u32, img.bytes.into_owned())
        {
            let dyn_img = DynamicImage::ImageRgba8(rgba);
            let encoded = encode_thumbnail_data_url(&dyn_img);
            if let Some(ref url) = encoded {
                shared.local_img_cache = Some(CachedLocalImage {
                    hash,
                    width: img.width,
                    height: img.height,
                    data_url: url.clone(),
                });
            }
            encoded
        } else {
            None
        };
        shared.snapshot.local_clipboard_preview = format!("{}×{} 像素", img.width, img.height);
        shared.snapshot.local_clipboard_image_data_url = data_url;
        shared.snapshot.local_clipboard_kind = format!("图片 · {}×{}", img.width, img.height);
        return;
    }
    shared.snapshot.local_clipboard_preview = String::new();
    shared.snapshot.local_clipboard_image_data_url = None;
    shared.snapshot.local_clipboard_kind = "空".into();
}

const _INDEX_HTML_WATCH: &str = include_str!("../../ui/index.html");

#[tauri::command]
fn start_window_drag(window: tauri::Window) {
    let _ = window.start_dragging();
}

#[tauri::command]
fn hide_main_window(window: tauri::Window) {
    let _ = window.hide();
}

#[tauri::command]
async fn get_app_state(app: AppHandle, state: State<'_, AppContext>) -> Result<AppStateSnapshot, String> {
    let autostart_enabled = app.autolaunch().is_enabled().unwrap_or(false);
    let mut guard = state.inner.lock().await;
    guard.snapshot.auto_sync = state.auto_sync.load(Ordering::Relaxed);
    guard.snapshot.autostart = autostart_enabled;
    refresh_local_os_clipboard(&mut guard);
    Ok(guard.snapshot.clone())
}

#[tauri::command]
async fn set_auto_sync(state: State<'_, AppContext>, enabled: bool) -> Result<AppStateSnapshot, String> {
    state.auto_sync.store(enabled, Ordering::Relaxed);
    if let Ok(lock) = state.sync_check_item.lock()
        && let Some(item) = lock.as_ref()
    {
        let _ = item.set_checked(enabled);
    }
    let mut guard = state.inner.lock().await;
    guard.snapshot.auto_sync = enabled;
    Ok(guard.snapshot.clone())
}

#[tauri::command]
async fn set_autostart(
    app: AppHandle,
    state: State<'_, AppContext>,
    enabled: bool,
) -> Result<AppStateSnapshot, String> {
    if enabled {
        app.autolaunch()
            .enable()
            .map_err(|e| format!("开启开机自启失败: {e}"))?;
    } else {
        app.autolaunch()
            .disable()
            .map_err(|e| format!("关闭开机自启失败: {e}"))?;
    }
    let actual = app.autolaunch().is_enabled().unwrap_or(enabled);
    if let Ok(lock) = state.autostart_check_item.lock()
        && let Some(item) = lock.as_ref()
    {
        let _ = item.set_checked(actual);
    }
    let mut guard = state.inner.lock().await;
    guard.snapshot.autostart = actual;
    Ok(guard.snapshot.clone())
}

#[tauri::command]
async fn set_preferred_peer(
    state: State<'_, AppContext>,
    name: String,
) -> Result<AppStateSnapshot, String> {
    let mut guard = state.inner.lock().await;
    let state_dir = guard.state_dir.clone();
    save_preferred_peer(&state_dir, &name);
    if let Some(client) = guard.client.clone() {
        refresh_devices_and_identity(&client, &state_dir, &mut guard.snapshot).await;
    }
    Ok(guard.snapshot.clone())
}

#[tauri::command]
async fn check_update(
    app: AppHandle,
    state: State<'_, AppContext>,
) -> Result<AppStateSnapshot, String> {
    let updater = app
        .updater()
        .map_err(|e| format!("初始化更新检查器失败: {e}"))?;
    match updater.check().await {
        Ok(Some(update)) => {
            let ver = update.version.clone();
            let mut guard = state.inner.lock().await;
            guard.snapshot.update_available = Some(ver.clone());
            guard.snapshot.banner = Some(format!("发现新版本 v{ver}！点击顶栏「立即升级」即可自动更新。"));
            guard.snapshot.banner_type = "info".into();
            Ok(guard.snapshot.clone())
        }
        Ok(None) => {
            let mut guard = state.inner.lock().await;
            guard.snapshot.update_available = None;
            guard.snapshot.banner = Some(format!(
                "当前已是最新版本（v{}）。",
                guard.snapshot.app_version
            ));
            guard.snapshot.banner_type = "info".into();
            Ok(guard.snapshot.clone())
        }
        Err(err) => {
            let mut guard = state.inner.lock().await;
            guard.snapshot.banner = Some(format!("检查更新失败：{err}"));
            guard.snapshot.banner_type = "error".into();
            Ok(guard.snapshot.clone())
        }
    }
}

#[tauri::command]
async fn install_update(
    app: AppHandle,
    state: State<'_, AppContext>,
) -> Result<AppStateSnapshot, String> {
    let updater = app
        .updater()
        .map_err(|e| format!("初始化更新器失败: {e}"))?;
    match updater.check().await {
        Ok(Some(update)) => {
            update
                .download_and_install(|_chunk_len, _total| {}, || {})
                .await
                .map_err(|e| format!("下载并安装更新失败: {e}"))?;
            app.restart();
        }
        Ok(None) => {
            let mut guard = state.inner.lock().await;
            guard.snapshot.update_available = None;
            guard.snapshot.banner = Some("当前已是最新版本，无需升级。".into());
            guard.snapshot.banner_type = "info".into();
            Ok(guard.snapshot.clone())
        }
        Err(err) => Err(format!("下载更新失败: {err}")),
    }
}

#[tauri::command]
async fn pull_now(state: State<'_, AppContext>) -> Result<AppStateSnapshot, String> {
    let got_new = poll_and_commit_once(&state).await?;
    let mut guard = state.inner.lock().await;
    if let Some(client) = guard.client.clone() {
        let state_dir = guard.state_dir.clone();
        refresh_devices_and_identity(&client, &state_dir, &mut guard.snapshot).await;
    }
    if got_new {
        guard.snapshot.banner = Some("已拉取最新内容并写入系统剪贴板。".into());
        guard.snapshot.banner_type = "info".into();
    } else {
        guard.snapshot.banner = Some("云端暂无未消费的新剪贴板内容。".into());
        guard.snapshot.banner_type = "info".into();
    }
    Ok(guard.snapshot.clone())
}

#[tauri::command]
async fn push_clipboard_to_phone(state: State<'_, AppContext>) -> Result<AppStateSnapshot, String> {
    let client = {
        let guard = state.inner.lock().await;
        guard.client.clone().ok_or("客户端尚未初始化")?
    };

    let mut clipboard = Clipboard::new().map_err(|e| format!("无法读取系统剪贴板: {e}"))?;
    if let Ok(text) = clipboard.get_text()
        && !text.is_empty()
    {
        client
            .send_text(&text)
            .await
            .map_err(|e| format!("发送文字到手机失败: {e}"))?;
        let mut guard = state.inner.lock().await;
        guard.snapshot.banner = Some("已把当前电脑剪贴板文字发送到手机微信输入法。".into());
        guard.snapshot.banner_type = "info".into();
        return Ok(guard.snapshot.clone());
    }

    if let Ok(img) = clipboard.get_image() {
        let rgba = image::RgbaImage::from_raw(
            img.width as u32,
            img.height as u32,
            img.bytes.into_owned(),
        )
        .ok_or("转换剪贴板图片像素失败")?;
        let temp_file = tempfile::Builder::new()
            .suffix(".png")
            .tempfile()
            .map_err(|e| format!("创建临时图片文件失败: {e}"))?;
        rgba.save(temp_file.path())
            .map_err(|e| format!("编码 PNG 失败: {e}"))?;
        client
            .send_image(temp_file.path())
            .await
            .map_err(|e| format!("发送图片到手机失败: {e}"))?;
        let mut guard = state.inner.lock().await;
        guard.snapshot.banner = Some("已把当前电脑剪贴板图片发送到手机微信输入法。".into());
        guard.snapshot.banner_type = "info".into();
        return Ok(guard.snapshot.clone());
    }

    Err("当前系统剪贴板里没有可发送的文字或图片".into())
}

#[tauri::command]
async fn pair_device(state: State<'_, AppContext>, code: String) -> Result<AppStateSnapshot, String> {
    let parsed_code = PairingCode::parse(code.trim()).map_err(|e| format!("配对码格式无效: {e}"))?;
    let client = {
        let guard = state.inner.lock().await;
        guard.client.clone().ok_or("客户端尚未初始化")?
    };

    let mut pairing = client
        .join(parsed_code)
        .await
        .map_err(|e| format!("发起配对请求失败: {e}"))?;
    let res = tokio::time::timeout(Duration::from_secs(90), pairing.wait()).await;
    match res {
        Ok(Ok((_group, peer))) => {
            let mut guard = state.inner.lock().await;
            let state_dir = guard.state_dir.clone();
            save_preferred_peer(&state_dir, &peer.name);
            refresh_devices_and_identity(&client, &state_dir, &mut guard.snapshot).await;
            guard.snapshot.banner = Some(format!("配对成功！已连接设备：{}", peer.name));
            guard.snapshot.banner_type = "info".into();
            Ok(guard.snapshot.clone())
        }
        Ok(Err(err)) => {
            let _ = pairing.cancel().await;
            Err(format!("{err}"))
        }
        Err(_) => {
            let _ = pairing.cancel().await;
            Err("等待手机确认超时，请重试".into())
        }
    }
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn setup_tray(app: &AppHandle, ctx: &AppContext) -> tauri::Result<()> {
    let open_item = MenuItem::with_id(app, "open", "打开 WePaste 面板...", true, None::<&str>)?;
    let sync_item = CheckMenuItem::with_id(
        app,
        "toggle_sync",
        "自动接收手机剪贴板",
        true,
        ctx.auto_sync.load(Ordering::Relaxed),
        None::<&str>,
    )?;
    if let Ok(mut guard) = ctx.sync_check_item.lock() {
        *guard = Some(sync_item.clone());
    }
    let autostart_enabled = app.autolaunch().is_enabled().unwrap_or(false);
    let autostart_item = CheckMenuItem::with_id(
        app,
        "toggle_autostart",
        "开机自动启动",
        true,
        autostart_enabled,
        None::<&str>,
    )?;
    if let Ok(mut guard) = ctx.autostart_check_item.lock() {
        *guard = Some(autostart_item.clone());
    }
    let pull_item = MenuItem::with_id(app, "pull_now", "立即从手机拉取一次", true, None::<&str>)?;
    let push_item = MenuItem::with_id(app, "push_now", "把电脑剪贴板发给手机", true, None::<&str>)?;
    let update_item = MenuItem::with_id(app, "check_update", "检查更新...", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let quit_item = MenuItem::with_id(app, "quit", "退出 WePaste", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &open_item,
            &sep1,
            &sync_item,
            &autostart_item,
            &pull_item,
            &push_item,
            &sep2,
            &update_item,
            &quit_item,
        ],
    )?;

    let ctx_clone = ctx.clone();
    let mut builder = TrayIconBuilder::new()
        .menu(&menu)
        .tooltip("WePaste — 微信输入法跨设备剪贴板同步")
        .show_menu_on_left_click(true)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "open" => show_main_window(app),
            "toggle_sync" => {
                let next = !ctx_clone.auto_sync.load(Ordering::Relaxed);
                ctx_clone.auto_sync.store(next, Ordering::Relaxed);
                if let Ok(lock) = ctx_clone.sync_check_item.lock()
                    && let Some(item) = lock.as_ref()
                {
                    let _ = item.set_checked(next);
                }
            }
            "toggle_autostart" => {
                let mgr = app.autolaunch();
                let next = !mgr.is_enabled().unwrap_or(false);
                if next {
                    let _ = mgr.enable();
                } else {
                    let _ = mgr.disable();
                }
                let actual = mgr.is_enabled().unwrap_or(next);
                if let Ok(lock) = ctx_clone.autostart_check_item.lock()
                    && let Some(item) = lock.as_ref()
                {
                    let _ = item.set_checked(actual);
                }
                let c = ctx_clone.clone();
                tauri::async_runtime::spawn(async move {
                    let mut guard = c.inner.lock().await;
                    guard.snapshot.autostart = actual;
                });
            }
            "pull_now" => {
                let c = ctx_clone.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = poll_and_commit_once(&c).await;
                });
            }
            "push_now" => {
                let c = ctx_clone.clone();
                tauri::async_runtime::spawn(async move {
                    let client_opt = c.inner.lock().await.client.clone();
                    if let Some(client) = client_opt
                        && let Ok(mut cb) = Clipboard::new()
                        && let Ok(text) = cb.get_text()
                        && !text.is_empty()
                    {
                        let _ = client.send_text(&text).await;
                    }
                });
            }
            "check_update" => {
                show_main_window(app);
                let app_handle = app.clone();
                let c = ctx_clone.clone();
                tauri::async_runtime::spawn(async move {
                    if let Ok(updater) = app_handle.updater() {
                        match updater.check().await {
                            Ok(Some(update)) => {
                                let ver = update.version.clone();
                                let mut guard = c.inner.lock().await;
                                guard.snapshot.update_available = Some(ver.clone());
                                guard.snapshot.banner = Some(format!(
                                    "发现新版本 v{ver}！可在「设备管理 / 设置」中一键升级。"
                                ));
                                guard.snapshot.banner_type = "info".into();
                            }
                            Ok(None) => {
                                let mut guard = c.inner.lock().await;
                                guard.snapshot.update_available = None;
                                guard.snapshot.banner = Some(format!(
                                    "当前已是最新版本 v{}。",
                                    env!("CARGO_PKG_VERSION")
                                ));
                                guard.snapshot.banner_type = "info".into();
                            }
                            Err(err) => {
                                let mut guard = c.inner.lock().await;
                                guard.snapshot.banner = Some(format!("检查更新失败: {err}"));
                                guard.snapshot.banner_type = "error".into();
                            }
                        }
                    }
                });
            }
            "quit" => {
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let _ = tray;
            }
        });

    #[cfg(target_os = "macos")]
    {
        if let Ok(template_icon) =
            tauri::image::Image::from_bytes(include_bytes!("../icons/tray-icon-template.png"))
        {
            builder = builder.icon(template_icon).icon_as_template(true);
        } else if let Some(icon) = app.default_window_icon().cloned() {
            builder = builder.icon(icon).icon_as_template(true);
        }
    }

    #[cfg(not(target_os = "macos"))]
    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }

    let _tray = builder.build(app)?;
    Ok(())
}

fn main() {
    let state_dir = resolve_state_dir();
    let device_name = resolve_device_name(&state_dir);

    let initial_snapshot = AppStateSnapshot {
        paired: false,
        auto_sync: true,
        autostart: false,
        platform: std::env::consts::OS.to_owned(),
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        update_available: None,
        local_name: device_name.clone(),
        local_uin: String::new(),
        last_preview: String::new(),
        last_image_data_url: None,
        last_kind: "—".into(),
        last_source: "—".into(),
        last_time: "暂无".into(),
        local_clipboard_preview: String::new(),
        local_clipboard_image_data_url: None,
        local_clipboard_kind: "—".into(),
        banner: None,
        banner_type: "info".into(),
        devices: Vec::new(),
    };

    let mut initial_shared = SharedData {
        client: None,
        state_dir,
        device_name,
        snapshot: initial_snapshot,
        local_img_cache: None,
    };
    refresh_local_os_clipboard(&mut initial_shared);

    let ctx = AppContext {
        auto_sync: Arc::new(AtomicBool::new(true)),
        inner: Arc::new(Mutex::new(initial_shared)),
        sync_check_item: Arc::new(std::sync::Mutex::new(None)),
        autostart_check_item: Arc::new(std::sync::Mutex::new(None)),
    };

    let ctx_for_setup = ctx.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(ctx)
        .on_page_load(|webview, payload| {
            if matches!(payload.event(), tauri::webview::PageLoadEvent::Finished) {
                let _ = webview.eval(
                    "if (!window.__WEPASTE_BOOTED__ && document.scripts && document.scripts[0]) { (0, eval)(document.scripts[0].textContent); }",
                );
            }
        })
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            #[cfg(target_os = "windows")]
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_decorations(false);
            }

            setup_tray(app.handle(), &ctx_for_setup)?;

            // 后台定期检查 GitHub Releases 新版本（启动 10 秒后首次检查，之后每 6 小时检查一次）
            let update_app_handle = app.handle().clone();
            let update_ctx = ctx_for_setup.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_secs(10)).await;
                loop {
                    if let Ok(updater) = update_app_handle.updater()
                        && let Ok(Some(update)) = updater.check().await
                    {
                        let mut guard = update_ctx.inner.lock().await;
                        guard.snapshot.update_available = Some(update.version);
                    }
                    tokio::time::sleep(Duration::from_secs(6 * 3600)).await;
                }
            });

            let bg_ctx = ctx_for_setup.clone();
            tauri::async_runtime::spawn(async move {
                let (state_dir, device_name) = {
                    let guard = bg_ctx.inner.lock().await;
                    (guard.state_dir.clone(), guard.device_name.clone())
                };

                match Client::open(ClientOptions::new(state_dir.clone(), device_name)).await {
                    Ok(client) => {
                        {
                            let mut guard = bg_ctx.inner.lock().await;
                            refresh_devices_and_identity(&client, &state_dir, &mut guard.snapshot)
                                .await;
                            guard.client = Some(client);
                        }

                        loop {
                            if bg_ctx.auto_sync.load(Ordering::Relaxed) {
                                let _ = poll_and_commit_once(&bg_ctx).await;
                            }
                            tokio::time::sleep(Duration::from_secs(2)).await;
                        }
                    }
                    Err(err) => {
                        let mut guard = bg_ctx.inner.lock().await;
                        guard.snapshot.banner = Some(format!("初始化微信输入法协议客户端失败: {err}"));
                        guard.snapshot.banner_type = "error".into();
                    }
                }
            });

            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                // 点关闭按钮时只隐藏窗口，保持菜单栏/系统托盘后台常驻同步
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            start_window_drag,
            hide_main_window,
            get_app_state,
            set_auto_sync,
            set_autostart,
            set_preferred_peer,
            check_update,
            install_update,
            pull_now,
            push_clipboard_to_phone,
            pair_device,
        ])
        .run(tauri::generate_context!())
        .expect("error while running WePaste");
}

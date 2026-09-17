//! 图标/显示名持久化缓存(iconcache.bin,"DFIC" v1)——从 ui.rs 原样搬出
//! (2026-09-08 ui.rs 拆分增量,纯搬家不改行为)。
//!
//! 冷启动加速核心:此前每次启动都对全部桌面条目跑 SHGFI 显示名解析 +
//! 图标提取(.lnk/exe 冷盘+杀软扫描单个可达数百 ms),这是"开机后栅栏比
//! 原生桌面晚好几秒"的主要可控来源。键与内存缓存一致:{path}\0{px};
//! 校验:mtime 与 raw 扫描一致 + 长度==4*px*px。

use std::collections::HashMap;

use crate::{logging, model, state};

pub(crate) fn icon_cache_path() -> std::path::PathBuf {
    model::config_dir().join("iconcache.bin")
}

/// 图标像素字节必须是 size×size×4(DIB 32bpp,见 render::icon_pixels),
/// 加载时逐条校验,不符即丢弃该条(防御旧版/损坏文件)。
const ICON_ENTRY_MAX_BYTES: usize = 4 * 256 * 256;

/// 解码 iconcache.bin 字节流(纯核,2026-09-16 提取供集成测试):
/// expect_mtime=本次扫描的 path→mtime(校验"路径在册且 mtime 一致")。
/// 头不符/版本不符/px 荒谬=整表丢弃;单条不合规只跳过该条。
pub fn decode_cache(
    buf: &[u8],
    expect_mtime: &HashMap<&str, u64>,
) -> (HashMap<String, Vec<u8>>, HashMap<String, String>) {
    let mut icons: HashMap<String, Vec<u8>> = HashMap::new();
    let mut names: HashMap<String, String> = HashMap::new();
    if buf.len() < 12 || &buf[0..4] != b"DFIC" {
        return Default::default();
    }
    let ver = u32::from_le_bytes(buf[4..8].try_into().unwrap_or([0; 4]));
    if ver != 1 {
        return Default::default();
    }
    let px = u32::from_le_bytes(buf[8..12].try_into().unwrap_or([0; 4]));
    // px 由调用方条目键的后缀再核一次;这里只挡住荒谬值
    if !(16..=256).contains(&px) {
        return Default::default();
    }
    let expected_len = (px as usize) * (px as usize) * 4;
    let count = u32::from_le_bytes(
        buf.get(12..16)
            .map(|s| s.try_into().unwrap_or([0; 4]))
            .unwrap_or([0; 4]),
    ) as usize;
    let mut off = 16usize;
    for _ in 0..count.min(8192) {
        if off + 2 > buf.len() {
            break;
        }
        let klen = u16::from_le_bytes(buf[off..off + 2].try_into().unwrap_or([0; 2])) as usize;
        off += 2;
        if klen == 0 || klen > 1024 || off + klen + 12 > buf.len() {
            break;
        }
        let key = String::from_utf8_lossy(&buf[off..off + klen]).to_string();
        off += klen;
        let mtime = u64::from_le_bytes(buf[off..off + 8].try_into().unwrap_or([0; 8]));
        off += 8;
        let blen = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap_or([0; 4])) as usize;
        off += 4;
        if blen > ICON_ENTRY_MAX_BYTES || off + blen > buf.len() {
            break;
        }
        // 键的路径部分必须存在于本次扫描且 mtime 一致(px 后缀也须匹配当前
        // DPI);单条不合规只跳过该条,不再中断整表。
        let path_part = key.split('\0').next().unwrap_or("");
        if blen == expected_len
            && key.ends_with(&format!("\0{px}"))
            && expect_mtime.get(path_part).copied() == Some(mtime)
            && mtime != 0
        {
            icons.insert(key, buf[off..off + blen].to_vec());
        }
        off += blen;
    }
    // 第二段:显示名表(path→display)。段头 magic 缺失不算错误(纯图标版兼容)。
    if off + 4 <= buf.len() && &buf[off..off + 4] == b"DFNM" {
        off += 4;
        if off + 4 <= buf.len() {
            let ncnt = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap_or([0; 4])) as usize;
            off += 4;
            for _ in 0..ncnt.min(8192) {
                if off + 2 > buf.len() {
                    break;
                }
                let plen =
                    u16::from_le_bytes(buf[off..off + 2].try_into().unwrap_or([0; 2])) as usize;
                off += 2;
                if plen == 0 || plen > 1024 || off + plen > buf.len() {
                    break;
                }
                let p = String::from_utf8_lossy(&buf[off..off + plen]).to_string();
                off += plen;
                if off + 2 > buf.len() {
                    break;
                }
                let dlen =
                    u16::from_le_bytes(buf[off..off + 2].try_into().unwrap_or([0; 2])) as usize;
                off += 2;
                if dlen > 512 || off + dlen > buf.len() {
                    break;
                }
                let d = String::from_utf8_lossy(&buf[off..off + dlen]).to_string();
                off += dlen;
                if !p.is_empty() && !d.is_empty() {
                    names.insert(p, d);
                }
            }
        }
    }
    (icons, names)
}

/// 编码 iconcache.bin 字节流(纯核,与 decode_cache 互逆):
/// entries=(key,mtime,bytes),names=(path,display);文件头单值 px。
pub fn encode_cache(
    entries: &[(String, u64, Vec<u8>)],
    names: &[(String, String)],
    px: u32,
) -> Vec<u8> {
    let total: usize = entries.iter().map(|e| e.2.len()).sum();
    let mut buf: Vec<u8> = Vec::with_capacity(total + 4096);
    buf.extend_from_slice(b"DFIC");
    buf.extend_from_slice(&1u32.to_le_bytes());
    buf.extend_from_slice(&px.to_le_bytes());
    buf.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for (key, mtime, bytes) in entries {
        buf.extend_from_slice(&(key.len() as u16).to_le_bytes());
        buf.extend_from_slice(key.as_bytes());
        buf.extend_from_slice(&mtime.to_le_bytes());
        buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        buf.extend_from_slice(bytes);
    }
    buf.extend_from_slice(b"DFNM");
    buf.extend_from_slice(&(names.len() as u32).to_le_bytes());
    for (p, d) in names {
        buf.extend_from_slice(&(p.len() as u16).to_le_bytes());
        buf.extend_from_slice(p.as_bytes());
        buf.extend_from_slice(&(d.len() as u16).to_le_bytes());
        buf.extend_from_slice(d.as_bytes());
    }
    buf
}

/// 加载持久化图标/显示名缓存,返回 (图标命中表, 显示名命中表)。
pub(crate) fn load_icon_cache_file(
    raw: &[model::FileItem],
) -> (HashMap<String, Vec<u8>>, HashMap<String, String>) {
    use std::io::Read;
    let mut f = match std::fs::File::open(icon_cache_path()) {
        Ok(f) => f,
        Err(_) => return Default::default(),
    };
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return Default::default();
    }
    let expect_mtime: HashMap<&str, u64> =
        raw.iter().map(|f| (f.path.as_str(), f.mtime_ms)).collect();
    let (icons, names) = decode_cache(&buf, &expect_mtime);
    logging::log(&format!(
        "boot icon cache loaded: icons={} names={}",
        icons.len(),
        names.len()
    ));
    (icons, names)
}

/// 把当前 icon_cache 与 files 的显示名快照落盘(tmp+rename 原子替换)。
/// 只收 px==当前系统图标像素 的条目(文件头单值 px,保证与加载端逐条
/// 长度校验一致);字节流恒为 px×px×4(render::icon_pixels 契约)。
/// ~56 项 ≈ 0.5MB,后台线程序列化无感知。由全局 tick 检测到提取计数
/// 变化后延迟调用——运行期懒提取(DPI 切换/新文件/残影预览)自动覆盖。
pub(crate) fn save_icon_cache_file_now(px_expected: u32) {
    // 1) 短暂持锁克隆快照
    let mut entries: Vec<(String, u64, std::sync::Arc<Vec<u8>>)> = Vec::new();
    let mut names: Vec<(String, String)> = Vec::new();
    {
        let s = state::state().lock().unwrap();
        let by_path: HashMap<&str, &model::FileItem> =
            s.files.iter().map(|f| (f.path.as_str(), f)).collect();
        for (key, buf) in s.icon_cache.iter() {
            let Some((p, pxs)) = key.split_once('\0') else {
                continue;
            };
            if pxs.parse::<u32>().ok() != Some(px_expected) {
                continue;
            }
            let blen = (px_expected as usize) * (px_expected as usize) * 4;
            if buf.len() != blen || blen > ICON_ENTRY_MAX_BYTES {
                continue;
            }
            let Some(fi) = by_path.get(p) else { continue };
            if fi.mtime_ms == 0 {
                continue;
            }
            entries.push((key.clone(), fi.mtime_ms, std::sync::Arc::new(buf.clone())));
        }
        for f in s.files.iter() {
            names.push((f.path.clone(), f.name.clone()));
        }
    }
    if entries.is_empty() {
        return;
    }
    if entries.len() > 512 {
        entries.sort_by_key(|(_, mt, _)| *mt);
        entries.drain(..entries.len() - 512);
    }
    // 2) 后台序列化(纯核 encode_cache)+写盘
    std::thread::spawn(move || {
        use std::io::Write;
        let entries: Vec<(String, u64, Vec<u8>)> = entries
            .iter()
            .map(|(k, m, b)| (k.clone(), *m, b.as_ref().clone()))
            .collect();
        let buf = encode_cache(&entries, &names, px_expected);
        let path = icon_cache_path();
        let tmp = path.with_extension("bin.tmp");
        let ok = std::fs::File::create(&tmp)
            .and_then(|mut f| {
                f.write_all(&buf)?;
                f.sync_all()
            })
            .and_then(|()| std::fs::rename(&tmp, &path))
            .is_ok();
        if !ok {
            logging::log("icon cache save failed");
        }
    });
}

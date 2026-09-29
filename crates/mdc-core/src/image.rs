//! 图片处理：海报裁剪（2:3 中心裁剪，后续加人脸定位）与下载。

use anyhow::{Context, Result};
use image::imageops::FilterType;
use std::path::Path;

/// 下载图片到本地。referer 用于通过站点防盗链（如图床校验）。
pub async fn download(
    url: &str,
    http: &reqwest::Client,
    dest: &Path,
    referer: Option<&str>,
) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut req = http.get(url);
    if let Some(r) = referer {
        req = req.header(reqwest::header::REFERER, r);
    }
    let bytes = req.send().await?.error_for_status()?.bytes().await?;
    std::fs::write(dest, &bytes)?;
    Ok(())
}

/// 中心裁剪为海报比例（2:3），输出 jpg。
///
/// 🔴 **不能用 `image::open()`**：它是**按文件扩展名**猜格式的，而调用方给的临时文件名
/// 固定是 `.jpg`。源站给 PNG / WebP 封面时就会解码失败 ——
/// 表现是「有的片子有海报、有的没有」，而且完全静默（错误被 `let _ =` 吞掉）。
/// 这里用 `with_guessed_format()` 按**文件内容的魔术字节**判断，与文件名无关。
///
/// 人脸定位裁剪在路线图 v0.3（引入 seetaface/rustface 模型后替换 center 参数）。
pub fn crop_poster(input: &Path, output: &Path) -> Result<()> {
    let file = std::fs::File::open(input)
        .with_context(|| format!("打开 {}", input.display()))?;
    let reader = image::ImageReader::new(std::io::BufReader::new(file))
        .with_guessed_format()
        .with_context(|| format!("探测图片格式失败：{}", input.display()))?;
    let img = reader
        .decode()
        .with_context(|| format!("解码失败（内容不是已知图片格式？）：{}", input.display()))?;
    let (w, h) = (img.width(), img.height());
    // 目标比例 2:3
    let target_w = if w * 3 > h * 2 {
        h * 2 / 3 // 太宽，裁宽度
    } else {
        w // 太高，裁高度（高度 = w*3/2）
    };
    let target_h = target_w * 3 / 2;
    let (target_h, target_w) = if target_h > h { (h, h * 2 / 3) } else { (target_h, target_w) };
    let x = w.saturating_sub(target_w) / 2;
    let y = h.saturating_sub(target_h) / 2;
    let cropped = img.crop_imm(x, y, target_w, target_h);
    // 宽度上限 800，控制体积
    let final_img = if cropped.width() > 800 {
        cropped.resize(800, u32::MAX, FilterType::Lanczos3)
    } else {
        cropped
    };
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    final_img.save_with_format(output, image::ImageFormat::Jpeg)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 手搓一张 24 位 BMP（60x90，2:3）。
    fn bmp_24(w: u32, h: u32) -> Vec<u8> {
        let row = (w * 3).div_ceil(4) * 4;
        let data_size = row * h;
        let file_size = 54 + data_size;
        let mut v: Vec<u8> = Vec::with_capacity(file_size as usize);
        v.extend_from_slice(b"BM");
        v.extend_from_slice(&file_size.to_le_bytes());
        v.extend_from_slice(&[0u8; 4]);
        v.extend_from_slice(&54u32.to_le_bytes());
        v.extend_from_slice(&40u32.to_le_bytes());
        v.extend_from_slice(&w.to_le_bytes());
        v.extend_from_slice(&h.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&24u16.to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes());
        v.extend_from_slice(&data_size.to_le_bytes());
        v.extend_from_slice(&[0u8; 16]);
        v.resize(file_size as usize, 0x80);
        v
    }

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("mdc_img_unit_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn crops_2_to_3() {
        let d = tmp("crop");
        let src = d.join("in.bmp");
        std::fs::write(&src, bmp_24(60, 90)).unwrap();
        let out = d.join("out.jpg");
        crop_poster(&src, &out).unwrap();
        let img = image::open(&out).unwrap();
        assert_eq!((img.width(), img.height()), (60, 90));
    }

    /// 🔴 内容格式与扩展名不一致时必须照样能裁。
    ///
    /// 调用方给的临时文件名**固定是 `.jpg`**，而源站可能给 PNG/WebP ——
    /// `image::open()` 按扩展名猜就会失败，海报静默消失。
    #[test]
    fn sniffs_content_not_extension() {
        let d = tmp("sniff");
        // 内容是 BMP，文件名却叫 .jpg —— 正是线上的情形
        let lying = d.join("cover.jpg");
        std::fs::write(&lying, bmp_24(60, 90)).unwrap();
        let out = d.join("poster.jpg");
        crop_poster(&lying, &out).expect("按内容判格式才对，不该因扩展名骗人而失败");
        assert!(out.exists());
    }

    /// 真给了非图片内容要报错，不能装作成功。
    #[test]
    fn garbage_input_errors() {
        let d = tmp("garbage");
        let bad = d.join("not-an-image.jpg");
        std::fs::write(&bad, b"this is definitely not an image").unwrap();
        assert!(crop_poster(&bad, &d.join("x.jpg")).is_err());
    }
}

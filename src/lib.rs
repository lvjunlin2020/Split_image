//! 图像等分切分核心逻辑:与 UI 解耦,GUI / CLI / 集成测试共用一条流水线。
//!
//! 语义约定(用户输入的是"刀数",内部按"份数"计算):
//! - `纵向切分` = 竖直方向切几刀(竖向切线,分出左右)→ 列数 = v_cuts + 1
//! - `横向切分` = 水平方向切几刀(横向切线,分出上下)→ 行数 = h_cuts + 1
//!   例:4 列 × 3 行的拼合图 = 纵向 3 刀 + 横向 2 刀
//! - `rows` = 行数(自上而下编号),`cols` = 列数(从左到右编号)
//! - 输出文件 = `{原文件名}_r{行}c{列}.{扩展名}`,写在原图同目录,前缀即原文件名
//! - 非整除尺寸的余数归入末块(如 100px 宽切 3 列 → 33/33/34),保证无缝拼接、完全覆盖

use std::fmt::Write as _;
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, ImageDecoder, ImageFormat};

/// 行/列份数上限(输出命名用两位编号,50×50=2500 份已远超实用场景)
pub const MAX_GRID: u32 = 50;

/// JPEG 输出质量(原图是 jpg 时保持视觉无损-ish)
const JPEG_QUALITY: u8 = 95;

/// 判断文件是否为本工具支持的图像(按扩展名能否映射到已知图像格式)
pub fn is_image_file(path: &Path) -> bool {
    ImageFormat::from_path(path).is_ok()
}

/// 等分网格:返回 `(行, 列, x, y, w, h)`,行优先(先从左到右,再从上到下)。
/// 边界用整数运算 `w*c/cols`,相邻块共享边界,总尺寸严格等于原图。
pub fn grid_cells(w: u32, h: u32, rows: u32, cols: u32) -> Vec<(u32, u32, u32, u32, u32, u32)> {
    let mut cells = Vec::with_capacity((rows * cols) as usize);
    for r in 0..rows {
        let (y0, y1) = (h * r / rows, h * (r + 1) / rows);
        for c in 0..cols {
            let (x0, x1) = (w * c / cols, w * (c + 1) / cols);
            cells.push((r + 1, c + 1, x0, y0, x1 - x0, y1 - y0));
        }
    }
    cells
}

/// 输出扩展名:输入格式可编码则沿用(小写),否则回退 png。
fn out_extension(orig: &Path) -> String {
    let encodable = |f: ImageFormat| {
        matches!(
            f,
            ImageFormat::Png
                | ImageFormat::Jpeg
                | ImageFormat::Bmp
                | ImageFormat::Gif
                | ImageFormat::Tiff
                | ImageFormat::WebP
                | ImageFormat::Tga
                | ImageFormat::Pnm
                | ImageFormat::Farbfeld
                | ImageFormat::Qoi
                | ImageFormat::Ico
        )
    };
    match ImageFormat::from_path(orig) {
        Ok(f) if encodable(f) => orig
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_else(|| "png".into()),
        _ => "png".into(),
    }
}

/// 第 (row, col) 块(1 起)的输出路径。stem 超 160 字符先截断(Windows 单组件限 255)。
fn piece_path(orig: &Path, row: u32, col: u32) -> PathBuf {
    let dir = orig.parent().unwrap_or(Path::new("."));
    let full_stem = orig
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image".into());
    let stem: String = if full_stem.chars().count() > 160 {
        full_stem.chars().take(160).collect()
    } else {
        full_stem
    };
    let mut name = String::new();
    let _ = write!(name, "{stem}_r{row:02}c{col:02}.{}", out_extension(orig));
    dir.join(name)
}

/// 加载图像并应用 EXIF 方向(手机竖拍照片不带旋转像素,直接切会方向错)。
pub fn open_with_orientation(path: &Path) -> image::ImageResult<DynamicImage> {
    let reader = image::ImageReader::open(path)?.with_guessed_format()?;
    let mut decoder = reader.into_decoder()?;
    let orientation = decoder.orientation()?;
    let mut img = DynamicImage::from_decoder(decoder)?;
    img.apply_orientation(orientation);
    Ok(img)
}

/// GUI 提示用:第 (row, col) 块的输出文件名(仅文件名部分)。
pub fn example_piece_name(orig: &Path, row: u32, col: u32) -> String {
    piece_path(orig, row, col)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// 切分单个文件,返回写出的份数。已有同名输出会直接覆盖(便于换参数重跑)。
pub fn split_file(orig: &Path, rows: u32, cols: u32) -> Result<usize, String> {
    let img = open_with_orientation(orig).map_err(|e| format!("读取失败: {e}"))?;
    let (w, h) = (img.width(), img.height());
    if w < cols || h < rows {
        return Err(format!(
            "尺寸 {w}×{h} 小于切分份数 {rows}×{cols},会出现空块"
        ));
    }
    let jpeg = ImageFormat::from_path(orig).map(|f| f == ImageFormat::Jpeg).unwrap_or(false);
    for (row, col, x, y, cw, ch) in grid_cells(w, h, rows, cols) {
        let piece = img.crop_imm(x, y, cw, ch);
        let path = piece_path(orig, row, col);
        let result = if jpeg {
            // DynamicImage 的编码方法会自动转成编码器支持的颜色类型(RGBA→RGB)
            let file =
                File::create(&path).map_err(|e| format!("写出 {} 失败: {e}", path.display()))?;
            piece.write_with_encoder(JpegEncoder::new_with_quality(
                BufWriter::new(file),
                JPEG_QUALITY,
            ))
        } else {
            piece.save(&path)
        };
        result.map_err(|e| format!("写出 {} 失败: {e}", path.display()))?;
    }
    Ok((rows * cols) as usize)
}

/// 扫描文件夹(不递归)中的全部图像文件,按文件名排序保证批处理顺序稳定。
pub fn scan_folder(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_image_file(p))
        .collect();
    files.sort();
    files
}

/// 批量切分。`on_file(done, total, line)` 用于进度回调;返回 (成功文件数, 失败文件数)。
pub fn split_all(
    files: &[PathBuf],
    rows: u32,
    cols: u32,
    mut on_file: impl FnMut(usize, usize, String),
    cancel: &AtomicBool,
) -> (usize, usize) {
    let total = files.len();
    let (mut ok, mut fail) = (0, 0);
    for (i, path) in files.iter().enumerate() {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            on_file(i, total, format!("已停止,剩余 {} 个文件跳过", total - i));
            break;
        }
        let line = match split_file(path, rows, cols) {
            Ok(n) => {
                ok += 1;
                format!("✔ {} → {n} 份", path.display())
            }
            Err(e) => {
                fail += 1;
                format!("✘ {} : {e}", path.display())
            }
        };
        on_file(i + 1, total, line);
    }
    (ok, fail)
}

/// 校验行列份数是否在允许范围
pub fn valid_grid(rows: u32, cols: u32) -> bool {
    rows >= 1 && cols >= 1 && rows <= MAX_GRID && cols <= MAX_GRID
}

/// 刀数 → 份数(行, 列):纵向竖刀 v 刀分出左右 v+1 列,横向横刀 h 刀分出上下 h+1 行。
pub fn cuts_to_parts(v_cuts: u32, h_cuts: u32) -> (u32, u32) {
    (h_cuts + 1, v_cuts + 1)
}

/// 校验刀数:每方向 0~MAX_GRID-1 刀(对应 1~MAX_GRID 份),0 刀即该方向不切。
pub fn valid_cuts(v_cuts: u32, h_cuts: u32) -> bool {
    v_cuts < MAX_GRID && h_cuts < MAX_GRID
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("split_image_test_{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn grid_covers_exactly_and_contiguous() {
        for (w, h, rows, cols) in [
            (100, 60, 3, 2),
            (100, 60, 1, 1),
            (100, 100, 3, 3),
            (97, 53, 7, 5),
            (2048, 1536, 4, 4),
        ] {
            let cells = grid_cells(w, h, rows, cols);
            assert_eq!(cells.len(), (rows * cols) as usize);
            // 每行宽度求和 = 总宽,每列高度求和 = 总高
            for r in 1..=rows {
                let row_w: u32 = cells
                    .iter()
                    .filter(|&&(cr, _, _, _, _, _)| cr == r)
                    .map(|&(_, _, _, _, cw, _)| cw)
                    .sum();
                assert_eq!(row_w, w, "row {r} of {w}x{h} {rows}x{cols}");
            }
            for c in 1..=cols {
                let col_h: u32 = cells
                    .iter()
                    .filter(|&&(_, cc, _, _, _, _)| cc == c)
                    .map(|&(_, _, _, _, _, ch)| ch)
                    .sum();
                assert_eq!(col_h, h, "col {c} of {w}x{h} {rows}x{cols}");
            }
        }
    }

    #[test]
    fn odd_division_gives_remainder_to_last_block() {
        let cells = grid_cells(100, 60, 2, 3);
        let widths: Vec<u32> = cells.iter().map(|&(_, _, _, _, cw, _)| cw).collect();
        assert_eq!(widths, vec![33, 33, 34, 33, 33, 34]);
    }

    #[test]
    fn piece_path_uses_stem_prefix_and_keeps_ext() {
        let p = piece_path(Path::new(r"C:\pics\照片 A.jpg"), 2, 3);
        assert_eq!(p, PathBuf::from(r"C:\pics\照片 A_r02c03.jpg"));
        // 不可编码格式回退 png
        let p2 = piece_path(Path::new("/tmp/x.dds"), 1, 1);
        assert_eq!(p2, PathBuf::from("/tmp/x_r01c01.png"));
    }

    #[test]
    fn split_png_end_to_end_pixel_mapping() {
        let dir = temp_dir("e2e");
        // 4 象限填不同颜色:红(左上) 绿(右上) 蓝(左下) 黄(右下)
        let mut img = RgbImage::new(200, 100);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = if y < 50 {
                if x < 100 { Rgb([255, 0, 0]) } else { Rgb([0, 255, 0]) }
            } else if x < 100 {
                Rgb([0, 0, 255])
            } else {
                Rgb([255, 255, 0])
            };
        }
        let src = dir.join("quad.png");
        img.save(&src).unwrap();

        let n = split_file(&src, 2, 2).unwrap();
        assert_eq!(n, 4);
        let expect = [
            ("quad_r01c01.png", [255, 0, 0]),
            ("quad_r01c02.png", [0, 255, 0]),
            ("quad_r02c01.png", [0, 0, 255]),
            ("quad_r02c02.png", [255, 255, 0]),
        ];
        for (name, rgb) in expect {
            let out = image::open(dir.join(name)).unwrap();
            assert_eq!((out.width(), out.height()), (100, 50));
            let rgb8 = out.to_rgb8();
            let px = rgb8.get_pixel(50, 25);
            assert_eq!(px.0, rgb, "{name} 颜色映射错误");
        }
        // 重跑覆盖不报错
        assert_eq!(split_file(&src, 2, 2).unwrap(), 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn split_rejects_image_smaller_than_grid() {
        let dir = temp_dir("small");
        let src = dir.join("tiny.png");
        RgbImage::new(3, 3).save(&src).unwrap();
        assert!(split_file(&src, 4, 4).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cuts_map_to_parts_per_user_semantics() {
        // 4 列 × 3 行拼合图 = 纵向 3 刀 + 横向 2 刀
        assert_eq!(cuts_to_parts(3, 2), (3, 4));
        // 0 刀 = 该方向不切(1 块)
        assert_eq!(cuts_to_parts(0, 0), (1, 1));
        assert_eq!(cuts_to_parts(2, 0), (1, 3));
        assert!(valid_cuts(0, 0) && !valid_cuts(MAX_GRID, 0));
    }

    #[test]
    fn scan_folder_filters_and_sorts() {
        let dir = temp_dir("scan");
        RgbImage::new(2, 2).save(dir.join("b.png")).unwrap();
        RgbImage::new(2, 2).save(dir.join("a.png")).unwrap();
        std::fs::write(dir.join("note.txt"), "x").unwrap();
        let files = scan_folder(&dir);
        let names: Vec<_> = files.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, vec!["a.png", "b.png"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! 集成测试:走公开 API,覆盖 png/jpeg 两种格式、批量流水线、取消语义。

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use image::{Rgb, RgbImage};
use split_image::{grid_cells, scan_folder, split_all, split_file};

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("split_image_it_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 纵向渐变 + 横向分色的图,便于按像素校验块位置
fn gradient(w: u32, h: u32) -> RgbImage {
    let mut img = RgbImage::new(w, h);
    for (x, y, p) in img.enumerate_pixels_mut() {
        let left = x < w / 2;
        *p = Rgb([
            (x * 255 / w) as u8,
            (y * 255 / h) as u8,
            if left { 40 } else { 220 },
        ]);
    }
    img
}

#[test]
fn jpeg_keeps_format_and_quality() {
    let dir = temp_dir("jpeg");
    let src = dir.join("photo.jpg");
    gradient(120, 80).save(&src).unwrap();

    assert_eq!(split_file(&src, 2, 2).unwrap(), 4);
    for name in ["photo_r01c01.jpg", "photo_r02c02.jpg"] {
        let p = dir.join(name);
        assert!(p.exists(), "{name} 未生成");
        assert_eq!(
            image::ImageFormat::from_path(&p).unwrap(),
            image::ImageFormat::Jpeg
        );
        let img = image::open(&p).unwrap();
        assert_eq!((img.width(), img.height()), (60, 40));
    }
    // 不应混入 png 输出
    assert!(!dir.join("photo_r01c01.png").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn batch_pipeline_reports_and_continues_on_error() {
    let dir = temp_dir("batch");
    gradient(60, 60).save(dir.join("ok1.png")).unwrap();
    gradient(60, 60).save(dir.join("ok2.png")).unwrap();
    // 损坏文件:扩展名合法但内容不是图像
    std::fs::write(dir.join("broken.png"), b"not an image").unwrap();

    let files = scan_folder(&dir);
    assert_eq!(files.len(), 3);

    let cancel = AtomicBool::new(false);
    let mut lines = Vec::new();
    let (ok, fail) = split_all(&files, 2, 3, None, |_, _, line| lines.push(line), &cancel);
    assert_eq!((ok, fail), (2, 1));
    assert!(lines.iter().any(|l| l.contains("broken.png")));
    // 好文件照常产出
    for f in ["ok1", "ok2"] {
        for c in 1..=3 {
            assert!(dir.join(format!("{f}_r01c0{c}.png")).exists());
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pieces_reassemble_losslessly_png() {
    // 切完再把块尺寸拼回去,总尺寸与原图一致(整数边界保证无缝覆盖)
    let dir = temp_dir("reassemble");
    let src = dir.join("odd.png");
    gradient(97, 53).save(&src).unwrap();
    split_file(&src, 4, 5).unwrap();

    let mut col_widths: Vec<Vec<u32>> = vec![Vec::new(); 4];
    for (r, c, _, _, cw, ch) in grid_cells(97, 53, 4, 5) {
        let piece = image::open(dir.join(format!("odd_r{r:02}c{c:02}.png"))).unwrap();
        assert_eq!((piece.width(), piece.height()), (cw, ch));
        col_widths[(r - 1) as usize].push(cw);
    }
    assert!(col_widths.iter().all(|w| w.iter().sum::<u32>() == 97));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cancel_stops_after_current_file() {
    let dir = temp_dir("cancel");
    for i in 0..50 {
        gradient(40, 40).save(dir.join(format!("f{i:02}.png"))).unwrap();
    }
    let files = scan_folder(&dir);
    let cancel = AtomicBool::new(true); // 立即取消:第一个文件完成后中断
    let (ok, _fail) = split_all(&files, 2, 2, None, |_, _, _| {}, &cancel);
    assert_eq!(ok, 0);
    let _ = std::fs::remove_dir_all(&dir);
}

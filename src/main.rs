//! 图像等分切分工具:图形界面(默认)+ 命令行模式(--cli)。
//!
//! 切分语义(按"刀数"理解):纵向切分 = 竖直方向切几刀(分出左右几列),
//! 横向切分 = 水平方向切几刀(分出上下几行)。4 列 × 3 行的拼合图 = 纵向 3 刀 + 横向 2 刀。
//! GUI:选单个图像文件或文件夹(批量),后台线程切分,
//! 输出以原文件名为前缀(`原名_r02c03.png`)保存在原图同目录。
//! CLI:`Split_image --cli <纵向刀数> <横向刀数> <文件或文件夹>...`(供自动化验证)。

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use eframe::egui;
use split_image::{cuts_to_parts, example_piece_name, scan_folder, split_all, valid_cuts, MAX_GRID};

/// 图标主色(浅蓝),用于界面强调色
const ACCENT: egui::Color32 = egui::Color32::from_rgb(96, 185, 238);

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--cli") {
        attach_parent_console();
        let code = run_cli(&args);
        std::process::exit(code);
    }
    if let Err(e) = run_gui() {
        eprintln!("GUI 启动失败: {e}");
        std::process::exit(1);
    }
}

/// 把输出接到启动本程序的父进程控制台(从 cmd 运行 --cli 时能看到输出)
#[cfg(windows)]
fn attach_parent_console() {
    use windows_sys::Win32::System::Console::AttachConsole;
    use windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS;
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}
#[cfg(not(windows))]
fn attach_parent_console() {}

/// 在资源管理器中打开目录(浏览切分结果)
#[cfg(windows)]
fn open_folder(path: &std::path::Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    let to_wide = |s: &str| -> Vec<u16> {
        std::ffi::OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
    };
    let file = to_wide(&path.to_string_lossy());
    let verb = to_wide("open");
    // HWND 是 isize(传 0);返回值 >32 才算成功
    let rc = unsafe {
        ShellExecuteW(0, verb.as_ptr(), file.as_ptr(), std::ptr::null(), std::ptr::null(), 1)
    };
    if rc <= 32 {
        Err(format!("ShellExecuteW 返回 {rc}"))
    } else {
        Ok(())
    }
}
#[cfg(not(windows))]
fn open_folder(path: &std::path::Path) -> Result<(), String> {
    let cmd = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    std::process::Command::new(cmd)
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

// ============================== CLI 模式 ==============================

fn run_cli(args: &[String]) -> i32 {
    let positional: Vec<&String> = args.iter().skip(1).filter(|a| *a != "--cli").collect();
    if positional.len() < 3 {
        eprintln!("用法: Split_image --cli <纵向刀数> <横向刀数> <图像文件或文件夹>...");
        return 2;
    }
    let (vcuts, hcuts): (u32, u32) = match (positional[0].parse(), positional[1].parse()) {
        (Ok(v), Ok(h)) if valid_cuts(v, h) => (v, h),
        _ => {
            eprintln!("刀数须为 0~{} 的整数(0 = 该方向不切)", MAX_GRID - 1);
            return 2;
        }
    };
    let (rows, cols) = cuts_to_parts(vcuts, hcuts);
    let mut files: Vec<PathBuf> = Vec::new();
    for p in &positional[2..] {
        let path = PathBuf::from(p.as_str());
        if path.is_dir() {
            files.extend(scan_folder(&path));
        } else if path.is_file() {
            files.push(path);
        } else {
            eprintln!("路径不存在: {p}");
            return 2;
        }
    }
    if files.is_empty() {
        eprintln!("未找到图像文件");
        return 2;
    }
    eprintln!("开始: {} 个文件,纵向 {vcuts} 刀 + 横向 {hcuts} 刀 → {rows} 行 × {cols} 列", files.len());
    let cancel = AtomicBool::new(false);
    let (ok, fail) = split_all(
        &files,
        rows,
        cols,
        |done, total, line| eprintln!("[{done}/{total}] {line}"),
        &cancel,
    );
    eprintln!("完成: 成功 {ok},失败 {fail}");
    if fail > 0 { 1 } else { 0 }
}

// ============================== GUI 模式 ==============================

enum Selection {
    None,
    File(PathBuf),
    Folder(PathBuf),
}

struct Thumb {
    dims: (u32, u32),
    texture: egui::TextureHandle,
}

enum Msg {
    Log(String),
    /// 单文件模式:后台解码的缩略图(失败时 Err)
    Thumb(Result<((u32, u32), egui::ColorImage), String>),
    Progress { done: usize, total: usize },
    Finished { ok: usize, fail: usize },
}

struct App {
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    ctx: egui::Context,
    selection: Selection,
    folder_files: Vec<PathBuf>,
    /// 纵向刀数(竖直切线 → 分出 vcuts+1 列)
    vcuts: u32,
    /// 横向刀数(水平切线 → 分出 hcuts+1 行)
    hcuts: u32,
    log: Vec<String>,
    busy: bool,
    cancel: Arc<AtomicBool>,
    progress: Option<(usize, usize)>,
    thumb: Option<Thumb>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_fonts(&cc.egui_ctx);
        setup_style(&cc.egui_ctx);
        let (tx, rx) = mpsc::channel();
        Self {
            tx,
            rx,
            ctx: cc.egui_ctx.clone(),
            selection: Selection::None,
            folder_files: Vec::new(),
            vcuts: 1,
            hcuts: 1,
            log: Vec::new(),
            busy: false,
            cancel: Arc::new(AtomicBool::new(false)),
            progress: None,
            thumb: None,
        }
    }

    fn log(&mut self, line: impl Into<String>) {
        self.log.push(line.into());
        if self.log.len() > 3000 {
            self.log.drain(..self.log.len() - 3000);
        }
    }

    fn select_file(&mut self, path: PathBuf) {
        self.selection = Selection::File(path.clone());
        self.folder_files.clear();
        self.thumb = None;
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Thumb(load_thumb(&path)));
            ctx.request_repaint();
        });
    }

    fn select_folder(&mut self, path: PathBuf) {
        self.selection = Selection::Folder(path.clone());
        self.thumb = None;
        self.folder_files = scan_folder(&path);
        self.log(format!(
            "文件夹 {} :找到 {} 个图像文件",
            path.display(),
            self.folder_files.len()
        ));
    }

    fn ready(&self) -> bool {
        match &self.selection {
            Selection::File(p) => p.is_file(),
            Selection::Folder(_) => !self.folder_files.is_empty(),
            Selection::None => false,
        }
    }

    fn start_split(&mut self) {
        let files: Vec<PathBuf> = match &self.selection {
            Selection::File(p) => vec![p.clone()],
            Selection::Folder(_) => self.folder_files.clone(),
            Selection::None => return,
        };
        if files.is_empty() {
            self.log("没有可处理的文件");
            return;
        }
        let (rows, cols) = cuts_to_parts(self.vcuts, self.hcuts);
        self.busy = true;
        self.progress = Some((0, files.len()));
        self.log(format!(
            "开始切分:{} 个文件,纵向 {} 刀 + 横向 {} 刀 → {rows} 行 × {cols} 列(每文件 {} 份)",
            files.len(),
            self.vcuts,
            self.hcuts,
            rows * cols
        ));
        self.cancel.store(false, Ordering::Relaxed);
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        let cancel = self.cancel.clone();
        std::thread::spawn(move || {
            let tx2 = tx.clone();
            let ctx2 = ctx.clone();
            let (ok, fail) = split_all(&files, rows, cols, move |done, total, line| {
                let _ = tx2.send(Msg::Log(line));
                let _ = tx2.send(Msg::Progress { done, total });
                ctx2.request_repaint();
            }, &cancel);
            let _ = tx.send(Msg::Finished { ok, fail });
            ctx.request_repaint();
        });
    }
}

/// 后台解码缩略图(最长边 480px)——大图解码不卡 UI
fn load_thumb(path: &std::path::Path) -> Result<((u32, u32), egui::ColorImage), String> {
    let img = split_image::open_with_orientation(path).map_err(|e| format!("无法读取: {e}"))?;
    let dims = (img.width(), img.height());
    let small = img.thumbnail(480, 480).to_rgba8();
    let size = [small.width() as usize, small.height() as usize];
    Ok((
        dims,
        egui::ColorImage::from_rgba_unmultiplied(size, &small.into_raw()),
    ))
}

/// 加载系统中文字体(微软雅黑),插入 Proportional 与 Monospace 首位
fn setup_fonts(ctx: &egui::Context) {
    let candidates = [
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\simhei.ttf",
        r"C:\Windows\Fonts\simsun.ttc",
    ];
    if let Some(p) = candidates.iter().find(|p| std::path::Path::new(p).exists()) {
        if let Ok(bytes) = std::fs::read(p) {
            let mut fonts = egui::FontDefinitions::default();
            fonts
                .font_data
                .insert("cjk".into(), egui::FontData::from_owned(bytes).into());
            for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                if let Some(list) = fonts.families.get_mut(&fam) {
                    list.insert(0, "cjk".into());
                }
            }
            ctx.set_fonts(fonts);
        }
    }
}

/// 深色主题 + 金黄强调色(呼应图标)
fn setup_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.visuals.selection.bg_fill = ACCENT.linear_multiply(0.35);
    style.visuals.selection.stroke.color = egui::Color32::WHITE;
    style.visuals.hyperlink_color = ACCENT;
    ctx.set_style(style);
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 每帧先清空消息队列
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Log(s) => self.log(s),
                Msg::Thumb(Ok((dims, image))) => {
                    let texture = ctx.load_texture("thumb", image, egui::TextureOptions::LINEAR);
                    self.thumb = Some(Thumb { dims, texture });
                }
                Msg::Thumb(Err(e)) => self.log(format!("预览加载失败:{e}")),
                Msg::Progress { done, total } => self.progress = Some((done, total)),
                Msg::Finished { ok, fail } => {
                    self.log(format!("完成:成功 {ok} 个文件,失败 {fail} 个"));
                    self.busy = false;
                    self.progress = None;
                }
            }
        }
        if self.busy {
            ctx.request_repaint_after(Duration::from_millis(150));
        }

        // 拖拽图像进窗口 = 选择该文件
        let dropped: Vec<PathBuf> = ctx
            .input(|i| i.raw.dropped_files.clone())
            .iter()
            .filter_map(|f| f.path.clone())
            .collect();
        if let Some(first) = dropped.into_iter().next() {
            if !self.busy {
                self.select_file(first);
            }
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading(
                egui::RichText::new("图像等分切分工具")
                    .strong()
                    .size(22.0)
                    .color(ACCENT),
            );
            ui.label(
                egui::RichText::new(
                    "选择图像(或直接拖入窗口);纵向=竖直切几刀(分出左右),横向=水平切几刀(分出上下);输出以原文件名为前缀,保存在原图同目录",
                )
                .weak(),
            );
            ui.add_space(8.0);

            // -------- 1. 选择来源 --------
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    let pick_file = ui.button("📄 选择图像文件…").clicked();
                    let pick_folder = ui.button("📁 选择文件夹(批量)…").clicked();
                    if pick_file && !self.busy {
                        if let Some(p) = rfd::FileDialog::new()
                            .add_filter(
                                "图像文件",
                                &["png", "jpg", "jpeg", "bmp", "gif", "tif", "tiff",
                                  "webp", "tga", "ico", "qoi", "ff", "pnm", "pgm",
                                  "ppm", "hdr", "dds", "exr"],
                            )
                            .pick_file()
                        {
                            self.select_file(p);
                        }
                    }
                    if pick_folder && !self.busy {
                        if let Some(p) = rfd::FileDialog::new().pick_folder() {
                            self.select_folder(p);
                        }
                    }
                });
                ui.add_space(4.0);
                match &self.selection {
                    Selection::None => {
                        ui.label(
                            egui::RichText::new("尚未选择 — 可选单个文件,或选文件夹批量处理其中所有图像")
                                .weak(),
                        );
                    }
                    Selection::File(p) => {
                        ui.label(format!("文件:{}", p.display()));
                        if let Some(t) = &self.thumb {
                            ui.label(format!("尺寸:{}×{} px", t.dims.0, t.dims.1));
                        }
                    }
                    Selection::Folder(p) => {
                        ui.label(format!("文件夹:{}", p.display()));
                        ui.label(format!("批量模式:将处理 {} 个图像文件", self.folder_files.len()));
                    }
                }
            });

            ui.add_space(8.0);
            // -------- 2. 切分参数(刀数语义:纵向竖刀分左右,横向横刀分上下) --------
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.label("纵向切分数(竖刀)");
                    ui.add(egui::DragValue::new(&mut self.vcuts).range(0..=MAX_GRID - 1).speed(0.2))
                        .on_hover_text("在竖直方向切几刀,刀数 N 分出左右 N+1 列。\n例:4 列拼合图 → 纵向 3 刀");
                    ui.separator();
                    ui.label("横向切分数(横刀)");
                    ui.add(egui::DragValue::new(&mut self.hcuts).range(0..=MAX_GRID - 1).speed(0.2))
                        .on_hover_text("在水平方向切几刀,刀数 N 分出上下 N+1 行。\n例:3 行拼合图 → 横向 2 刀");
                    ui.separator();
                    let (rows, cols) = cuts_to_parts(self.vcuts, self.hcuts);
                    ui.label(
                        egui::RichText::new(format!("→ {rows} 行 × {cols} 列 = {} 份/文件", rows * cols))
                            .strong()
                            .color(ACCENT),
                    );
                });
                if let Selection::File(p) = &self.selection {
                    let (rows, cols) = cuts_to_parts(self.vcuts, self.hcuts);
                    let example = example_piece_name(p, rows.min(2), cols.min(2));
                    ui.label(
                        egui::RichText::new(format!(
                            "输出示例:{example}(保存在原图同目录)"
                        ))
                        .weak(),
                    )
                    .on_hover_text("命名规则:原文件名_r行c列.扩展名;重跑会覆盖同名输出");
                }
            });

            ui.add_space(8.0);
            // -------- 3. 网格预览(所见即所得) --------
            ui.group(|ui| {
                let (rows, cols) = cuts_to_parts(self.vcuts, self.hcuts);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("切分预览").strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "纵向 {} 刀 + 横向 {} 刀 → {rows} 行 × {cols} 列",
                                self.vcuts, self.hcuts
                            ))
                            .weak(),
                        );
                    });
                });
                let avail = ui.available_size();
                let preview_h = (avail.y - 150.0).clamp(140.0, 320.0);
                let (rect, _resp) =
                    ui.allocate_exact_size(egui::vec2(avail.x, preview_h), egui::Sense::hover());
                let painter = ui.painter_at(rect);
                painter.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);

                // 图像区(保持宽高比,居中)
                let (iw, ih) = match &self.thumb {
                    Some(t) => (t.dims.0 as f32, t.dims.1 as f32),
                    None => (4.0, 3.0),
                };
                let scale = ((rect.width() - 16.0) / iw).min((rect.height() - 16.0) / ih);
                let size = egui::vec2(iw * scale, ih * scale);
                let img_rect = egui::Rect::from_center_size(rect.center(), size);
                if let Some(t) = &self.thumb {
                    painter.image(
                        t.texture.id(),
                        img_rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                } else {
                    painter.rect_filled(img_rect, 2.0, egui::Color32::from_gray(48));
                    painter.text(
                        img_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "无预览(未选择或文件夹批量)",
                        egui::FontId::proportional(14.0),
                        egui::Color32::from_gray(120),
                    );
                }

                // 网格线:画在 c/cols、r/rows 的精确分数位置(与实际切分一致)
                let line = egui::Stroke::new(1.5_f32, ACCENT);
                for c in 1..cols {
                    let x = img_rect.left() + img_rect.width() * c as f32 / cols as f32;
                    painter.line_segment(
                        [egui::pos2(x, img_rect.top()), egui::pos2(x, img_rect.bottom())],
                        line,
                    );
                }
                for r in 1..rows {
                    let y = img_rect.top() + img_rect.height() * r as f32 / rows as f32;
                    painter.line_segment(
                        [egui::pos2(img_rect.left(), y), egui::pos2(img_rect.right(), y)],
                        line,
                    );
                }
                painter.rect_stroke(img_rect, 2.0, egui::Stroke::new(2.0_f32, ACCENT));

                // 块编号(份数少时才标,避免糊成一团);r=行 c=列,均从 1 开始
                if rows * cols <= 36 {
                    let th = img_rect.height() / rows as f32;
                    let tw = img_rect.width() / cols as f32;
                    for r in 1..=rows {
                        for c in 1..=cols {
                            let cx = img_rect.left() + tw * (c as f32 - 0.5);
                            let cy = img_rect.top() + th * (r as f32 - 0.5);
                            let font = egui::FontId::proportional((th.min(18.0) - 2.0).max(7.0));
                            painter.text(
                                egui::pos2(cx, cy),
                                egui::Align2::CENTER_CENTER,
                                format!("r{r:02}c{c:02}"),
                                font,
                                egui::Color32::WHITE,
                            );
                        }
                    }
                }
            });

            ui.add_space(8.0);
            // -------- 4. 执行 + 进度 --------
            ui.horizontal(|ui| {
                let can_start = !self.busy && self.ready() && valid_cuts(self.vcuts, self.hcuts);
                let start = ui.add_sized(
                    [150.0, 32.0],
                    egui::Button::new(egui::RichText::new(if self.busy {
                        "切分中…"
                    } else {
                        "开始切分"
                    })
                    .strong()),
                );
                if start.clicked() && can_start {
                    self.start_split();
                }
                if !can_start && !self.busy && !self.ready() {
                    ui.label(egui::RichText::new("请先选择文件或文件夹").weak());
                }
                if self.busy {
                    if ui.button("停止").clicked() {
                        self.cancel.store(true, Ordering::Relaxed);
                        self.log("正在停止(完成当前文件后中断)…");
                    }
                    ui.spinner();
                }
                // 浏览切分结果:打开输出所在目录(单文件=原图目录,批量=所选文件夹)
                let out_dir: Option<PathBuf> = match &self.selection {
                    Selection::File(p) => p.parent().map(|d| d.to_path_buf()),
                    Selection::Folder(d) => Some(d.clone()),
                    Selection::None => None,
                };
                let browse =
                    ui.add_enabled(out_dir.is_some(), egui::Button::new("📂 浏览切分结果"));
                if browse.clicked() {
                    if let Some(dir) = &out_dir {
                        if let Err(e) = open_folder(dir) {
                            self.log(format!("打开目录失败:{e}"));
                        }
                    }
                }
                browse.on_hover_text("在资源管理器中打开切分结果的保存目录(原图所在目录)");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).weak());
                });
            });
            if let Some((done, total)) = self.progress {
                ui.add(
                    egui::ProgressBar::new(done as f32 / total.max(1) as f32)
                        .text(format!("{done} / {total} 文件")),
                );
            }

            ui.add_space(4.0);
            ui.separator();
            // -------- 5. 日志 --------
            egui::ScrollArea::vertical()
                .stick_to_bottom(true)
                .max_height(140.0)
                .show(ui, |ui| {
                    if self.log.is_empty() {
                        ui.label(egui::RichText::new("日志将显示在这里").weak());
                    }
                    for line in &self.log {
                        ui.monospace(egui::RichText::new(line).weak().size(12.0));
                    }
                });
        });
    }
}

fn run_gui() -> eframe::Result<()> {
    let icon = egui::IconData {
        rgba: include_bytes!("../assets/icon_64.rgba").to_vec(),
        width: 64,
        height: 64,
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("图像等分切分工具")
            .with_inner_size([820.0, 760.0])
            .with_min_inner_size([680.0, 620.0])
            .with_icon(icon),
        ..Default::default()
    };
    eframe::run_native(
        "Split_image",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

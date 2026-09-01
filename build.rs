//! 把图标与版本信息编译进 exe 资源。
//! windows-gnu 工具链需要 windres.exe(在 D:\mingw64\bin,source env.sh 已配)。
//! assets/icon.ico 不存在时自动跳过图标,不阻塞编译。

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap() != "windows" {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    if std::path::Path::new("assets/icon.ico").exists() {
        res.set_icon("assets/icon.ico");
    }
    res.set("ProductName", env!("CARGO_PKG_NAME"));
    res.set("FileVersion", env!("CARGO_PKG_VERSION"));
    res.set("ProductVersion", env!("CARGO_PKG_VERSION"));
    res.compile().expect("资源编译失败(需要 windres.exe 在 PATH:D:\\mingw64\\bin)");
}

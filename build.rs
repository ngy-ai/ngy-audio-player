//! 把图标写进 Windows 可执行文件。
//!
//! GPUI 在 Windows 上只从 exe 的资源 ID 1 读窗口 / 任务栏图标，
//! 运行时 `WindowOptions::icon` 在那里不生效，所以必须走资源文件。

fn main() {
    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    embed_resource::compile(
        "assets/app.rc",
        embed_resource::ParamsIncludeDirs(["assets"]),
    )
    .manifest_optional()
    .expect("编译 Windows 图标资源失败");
}

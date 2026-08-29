//! 构建脚本：向 exe 嵌入 comctl32 v6 视觉样式清单（SxS manifest）
//!
//! 没有 manifest 时，进程加载 comctl32 v5（Win95 经典控件外观）：
//! EDIT 是 3D 凹陷边框、BUTTON 是灰色凸起按钮、ListView 是老式报表。
//! 声明依赖 `Microsoft.Windows.Common-Controls` 6.0 后，系统加载 v6
//! 公共控件库，所有标准控件切到现代视觉样式，也是 `SetWindowTheme`
//! （如 DarkMode_Explorer）能对控件生效的前提（决策记录 D11）。
//!
//! 仅在目标为 Windows 时编译资源，其他平台构建为空操作。

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }

    let manifest = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity
        type="win32"
        name="Microsoft.Windows.Common-Controls"
        version="6.0.0.0"
        processorArchitecture="*"
        publicKeyToken="6595b64144ccf1df"
        language="*"
      />
    </dependentAssembly>
  </dependency>
</assembly>
"#;

    let mut resource = winresource::WindowsResource::new();
    resource.set_manifest(manifest);
    if let Err(e) = resource.compile() {
        panic!("嵌入视觉样式清单失败: {e}");
    }
}

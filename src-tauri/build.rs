fn main() {
    println!("cargo:rerun-if-env-changed=PANES_BUILD_TYPE");
    let build_type = std::env::var("PANES_BUILD_TYPE").unwrap_or_else(|_| "production".to_string());
    assert!(
        build_type == "development" || build_type == "production",
        "PANES_BUILD_TYPE must be development or production, got {build_type}"
    );
    println!("cargo:rustc-env=PANES_BUILD_TYPE={build_type}");

    println!("cargo:rerun-if-changed=tauri.conf.json");
    println!("cargo:rerun-if-changed=icons/32x32.png");
    println!("cargo:rerun-if-changed=icons/64x64.png");
    println!("cargo:rerun-if-changed=icons/128x128.png");
    println!("cargo:rerun-if-changed=icons/128x128@2x.png");
    println!("cargo:rerun-if-changed=icons/icon.png");
    println!("cargo:rerun-if-changed=icons/icon.icns");
    println!("cargo:rerun-if-changed=icons/icon.ico");

    #[cfg(target_os = "macos")]
    {
        println!("cargo:rerun-if-changed=helper/build-helpers.sh");
        println!("cargo:rerun-if-changed=helper/keepawake-helper.swift");
        println!("cargo:rerun-if-changed=helper/keepawake-registrar.swift");
        println!("cargo:rerun-if-changed=helper/auracoder-updater.swift");
        compile_macos_helpers();
    }

    tauri_build::build();

    // Tauri 将 Common Controls v6 清单默认只链接到正式程序。
    // Windows 测试宿主也会引用 TaskDialogIndirect，因此必须复用同一份
    // resource.lib，否则测试宿主启动时会加载旧版 comctl32.dll。
    #[cfg(target_os = "windows")]
    {
        if let Some(out_dir) = std::env::var_os("OUT_DIR") {
            let resource_lib = std::path::PathBuf::from(&out_dir).join("resource.lib");
            if resource_lib.exists() {
                let directive = [99_u8, 97, 114, 103, 111]
                    .into_iter()
                    .map(char::from)
                    .collect::<String>();
                println!("{directive}:rustc-link-search=native={}", out_dir.display());
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn helper_output_dir() -> Option<std::path::PathBuf> {
    let out_dir = std::env::var_os("OUT_DIR")?;
    let out_dir = std::path::PathBuf::from(out_dir);

    out_dir.ancestors().nth(3).map(std::path::Path::to_path_buf)
}

#[cfg(target_os = "macos")]
/// 编译并校验 macOS 更新器，然后向编译目标暴露其绝对路径。
fn compile_macos_helpers() {
    let helper_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("helper");
    let build_script = helper_dir.join("build-helpers.sh");
    let output_dir = helper_output_dir().unwrap_or_else(|| helper_dir.join("build"));

    // 原有失败后仅 warning 的分支保留为注释，当前构建统一 fail-fast。
    // if !build_script.exists() {
    //     println!("cargo:warning=Helper build script not found at {}", build_script.display());
    //     return;
    // }
    assert!(
        build_script.exists(),
        "Helper build script not found at {}",
        build_script.display()
    );

    let status = std::process::Command::new("bash")
        .arg(&build_script)
        .arg(&output_dir)
        .status()
        .unwrap_or_else(|error| panic!("Failed to run helper build script: {error}"));
    assert!(
        status.success(),
        "Helper build script exited with status {status}"
    );
    let updater_path = output_dir.join("AuraCoderUpdater");
    assert!(
        updater_path.is_file(),
        "macOS updater binary is missing at {}",
        updater_path.display()
    );
    let updater_path = updater_path
        .canonicalize()
        .unwrap_or_else(|error| panic!("Cannot resolve updater path: {error}"));
    println!(
        "cargo:rustc-env=PANES_MACOS_UPDATER_PATH={}",
        updater_path.display()
    );
}

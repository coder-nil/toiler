use super::*;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let name = format!(
            "toiler npm test {} {}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = std::env::temp_dir().join(name);
        std::fs::create_dir(&dir).unwrap();
        Self(dir)
    }

    fn dir(&self, subdir: &str) -> PathBuf {
        let dir = self.0.join(subdir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn orders_installed_nvm_versions_numerically() {
    let fixture = Fixture::new();
    let dir = |version: &str| {
        if cfg!(windows) {
            fixture.dir(version)
        } else {
            fixture.dir(&format!("versions/node/{version}/bin"))
        }
    };
    let nine = dir("v22.9.0");
    let ten = dir("v22.10.0");
    let newer = dir("v24.0.0");
    dir("v25.not-a-version");
    assert_eq!(
        nvm_bins(&fixture.0),
        (false, vec![newer.clone(), ten.clone(), nine.clone()])
    );

    #[cfg(unix)]
    {
        let aliases = fixture.dir("alias/lts");
        std::fs::write(aliases.join("jod"), "v22.9.0\n").unwrap();
        std::fs::write(aliases.join("*"), "lts/jod\n").unwrap();
        let default = fixture.0.join("alias/default");
        std::fs::write(&default, "lts/*\n").unwrap();
        assert_eq!(
            nvm_bins(&fixture.0),
            (false, vec![nine.clone(), newer.clone(), ten.clone()])
        );
        std::fs::write(&default, "22\n").unwrap();
        assert_eq!(
            nvm_bins(&fixture.0),
            (false, vec![ten.clone(), nine.clone(), newer.clone()])
        );
        std::fs::write(&default, "system\n").unwrap();
        assert_eq!(nvm_bins(&fixture.0), (true, vec![newer, ten, nine]));
    }
}

#[cfg(unix)]
fn script(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn uses_active_path_before_system_and_skips_broken_pairs() {
    let fixture = Fixture::new();
    let broken = fixture.dir("broken");
    let active = fixture.dir("custom nvm/versions/node/v22.10.0/bin");
    let system = fixture.dir("system");
    for bin in [&broken, &active, &system] {
        script(&bin.join(NODE), "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo v22.10.0; else exec /bin/sh \"$@\"; fi\n");
        script(
            &bin.join("npm"),
            "#!/usr/bin/env node\nprintf '10.9.0\\n'\n",
        );
    }
    script(&broken.join("npm"), "#!/bin/sh\nexit 1\n");
    let mut search = Search {
        override_npm: None,
        path: vec![broken.clone(), active.clone()],
        active_nvm: None,
        nvm_root: Some(fixture.0.join("custom nvm")),
        system: vec![system.clone()],
    };
    let runtime = search.resolve().await.unwrap();
    assert_eq!(runtime.node, active.join(NODE));
    assert_eq!(search.source(&runtime.node), "nvm");
    assert_eq!(
        std::env::split_paths(&runtime.path).next(),
        Some(active.clone())
    );

    // 模拟桌面启动没有 Node 的 PATH；仍能由自定义 nvm 根找到一对。
    search.path.clear();
    assert_eq!(search.resolve().await.unwrap().node, active.join(NODE));

    // nvm 的 node 损坏时再尝试系统安装，不执行那些版本的实际装包命令。
    script(&active.join(NODE), "#!/bin/sh\nexit 1\n");
    assert_eq!(search.resolve().await.unwrap().node, system.join(NODE));

    // 显式覆盖是用户选择，坏了要指出配置，不能静默改用其他 npm。
    search.override_npm = Some(broken.join("npm"));
    assert!(matches!(search.resolve().await, Err(error) if error.contains("TOILER_NPM")));
}

#[cfg(unix)]
#[tokio::test]
async fn pairs_a_split_system_install() {
    let fixture = Fixture::new();
    let npm_bin = fixture.dir("npm bin");
    let node_bin = fixture.dir("node bin");
    script(&npm_bin.join("npm"), "#!/usr/bin/env node\necho 10.1.0\n");
    script(&node_bin.join(NODE), "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo v22.10.0; else exec /bin/sh \"$@\"; fi\n");
    let search = Search {
        override_npm: None,
        path: vec![npm_bin.clone()],
        active_nvm: None,
        nvm_root: None,
        system: vec![node_bin.clone()],
    };
    let runtime = search.resolve().await.unwrap();
    assert_eq!(runtime.npm, npm_bin.join("npm"));
    assert_eq!(runtime.node, node_bin.join(NODE));
}

#[cfg(windows)]
#[test]
fn windows_launcher_uses_node_and_preserves_arguments() {
    use std::ffi::OsStr;

    let fixture = Fixture::new();
    let npm = fixture.0.join("npm.cmd");
    assert_eq!(npm_cli(&npm), None, "没有 CLI 的批处理不能执行");
    let cli = fixture.dir("node_modules/npm/bin").join("npm-cli.js");
    std::fs::write(&cli, "").unwrap();
    let node = fixture.0.join(NODE);
    let runtime = NpmRuntime {
        cli: npm_cli(&npm).unwrap(),
        npm,
        node: node.clone(),
        path: std::env::join_paths([&fixture.0]).unwrap(),
    };
    let mut command = runtime.command();
    command.args([
        "install",
        "@scope/plugin@^1.2.3",
        "--prefix",
        "C:\\path with spaces",
    ]);
    let cmd = command.as_std();
    assert_eq!(cmd.get_program(), node.as_os_str());
    // Windows 将 `/` 和 `\` 都视为路径分隔符，只有脚本路径按路径语义比较。
    let mut args = cmd.get_args();
    assert_eq!(args.next().map(Path::new), Some(cli.as_path()));
    assert_eq!(
        args.collect::<Vec<_>>(),
        [
            OsStr::new("install"),
            OsStr::new("@scope/plugin@^1.2.3"),
            OsStr::new("--prefix"),
            OsStr::new("C:\\path with spaces"),
        ]
    );
}

#[tokio::test]
#[ignore = "需要在 PATH 安装真实 Node.js 和 npm；由发布 CI 显式运行"]
async fn native_node_and_npm_can_run() {
    // 只跑版本探测，不联网或安装插件；Windows 在这里实际执行 node.exe + npm-cli.js。
    let search = Search {
        override_npm: None,
        active_nvm: None,
        nvm_root: None,
        system: Vec::new(),
        ..Search::from_env()
    };
    let runtime = search
        .resolve()
        .await
        .expect("应能检测到 PATH 中的 Node.js 和 npm");
    assert!(runtime.node.is_absolute());
    assert!(runtime.npm.is_absolute());
    #[cfg(windows)]
    assert!(runtime.cli.is_some(), "应覆盖标准 npm.cmd 布局");
}

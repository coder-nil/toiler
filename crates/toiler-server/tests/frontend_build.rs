//! 对应桌面启动后的真实静态页面：先嵌占位页再构建前端时，Cargo 必须重新选择 dist。
//! 用无依赖的隔离小 crate 执行真实 build.rs，避免只断言源码字符串却漏掉 Cargo 缓存行为。

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

struct Fixture {
    root: PathBuf,
    server: PathBuf,
    frontend: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "toiler-frontend-build-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let server = root.join("crates/toiler-server");
        let frontend = root.join("crates/toiler-desktop/frontend");
        std::fs::create_dir_all(server.join("src")).unwrap();
        std::fs::create_dir_all(server.join("frontend")).unwrap();
        std::fs::create_dir_all(&frontend).unwrap();
        std::fs::write(
            server.join("Cargo.toml"),
            "[package]\nname = \"frontend-build-probe\"\nversion = \"0.0.0\"\nedition = \"2021\"\n[workspace]\n",
        )
        .unwrap();
        std::fs::write(server.join("build.rs"), include_str!("../build.rs")).unwrap();
        std::fs::write(
            server.join("src/main.rs"),
            "fn main() { println!(\"{}\", env!(\"TOILER_FRONTEND_SOURCE\")); println!(\"{}\", env!(\"TOILER_FRONTEND_DIR\")); }\n",
        )
        .unwrap();
        std::fs::write(server.join("frontend/index.html"), "占位页").unwrap();
        Self {
            root,
            server,
            frontend,
        }
    }

    fn run(&self, override_dir: Option<&Path>) -> Output {
        let mut command = Command::new(env!("CARGO"));
        command
            .args(["run", "--offline", "--quiet", "--manifest-path"])
            .arg(self.server.join("Cargo.toml"))
            .env("CARGO_TARGET_DIR", self.root.join("target"))
            .env_remove("CARGO_MAKEFLAGS")
            .env_remove("TOILER_FRONTEND_DIR");
        if let Some(dir) = override_dir {
            command.env("TOILER_FRONTEND_DIR", dir);
        }
        command.output().unwrap()
    }

    fn assert_source(&self, source: &str, dir: &Path) {
        let output = self.run(None);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout).unwrap();
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some(source));
        assert_eq!(
            PathBuf::from(lines.next().unwrap()),
            dir.canonicalize().unwrap()
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn cargo_switches_from_placeholder_when_frontend_is_created() {
    let fixture = Fixture::new();
    fixture.assert_source("builtin", &fixture.server.join("frontend"));

    // 跨过粗粒度文件系统的时间戳边界，确保测的是候选目录监听，不是计时巧合。
    std::thread::sleep(Duration::from_millis(1100));
    let dist = fixture.frontend.join("dist");
    std::fs::create_dir_all(&dist).unwrap();
    std::fs::write(dist.join("index.html"), "真实前端").unwrap();
    fixture.assert_source("frontend-dist", &dist);

    // 构建器会清空 dist；产物被移走时也应重新判定，不能继续报告旧目录。
    std::thread::sleep(Duration::from_millis(1100));
    std::fs::remove_file(dist.join("index.html")).unwrap();
    fixture.assert_source("builtin", &fixture.server.join("frontend"));
}

#[test]
fn explicit_frontend_override_is_checked_and_can_be_removed() {
    let fixture = Fixture::new();
    let external = fixture.root.join("external");
    std::fs::create_dir_all(&external).unwrap();
    std::fs::write(external.join("index.html"), "外部产物").unwrap();
    let output = fixture.run(Some(&external));
    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .starts_with("TOILER_FRONTEND_DIR\n"));

    fixture.assert_source("builtin", &fixture.server.join("frontend"));
    let invalid = fixture.run(Some(&fixture.root.join("missing")));
    assert!(!invalid.status.success(), "无效显式路径不能静默回退");
}

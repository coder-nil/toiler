//! 检查 models.json → rpi provider 的映射（不发请求）。
use toiler_server::catalog::ModelCatalog;

fn main() -> anyhow::Result<()> {
    let catalog = ModelCatalog::load()?;
    println!("来源: {:?}", catalog.source);
    println!(
        "模型: {} 个，provider: {} 个",
        catalog.entries.len(),
        catalog.providers.len()
    );
    for e in catalog.entries.iter().take(12) {
        println!(
            "  {:14} {:28} api={:18} ctx={:>7} reasoning={}",
            e.provider, e.id, e.api, e.context_window, e.reasoning
        );
    }
    match catalog.default_model() {
        Some(m) => println!("默认模型: {}/{}", m.provider.as_str(), m.id),
        None => println!("默认模型: 无"),
    }
    Ok(())
}

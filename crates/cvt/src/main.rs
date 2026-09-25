use std::time::Duration;

#[derive(serde::Deserialize, serde::Serialize, Debug)]
struct Version {
    version: String,
    #[serde(default)]
    meta: Option<bool>,
}

async fn http_smoke() -> anyhow::Result<()> {
    let c = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let v: Version = c
        .get("https://crates.io/api/v1/crates/ratatui")
        .header("User-Agent", "cvt-smoke")
        .send()
        .await?
        .json()
        .await?;
    let patch = c.patch("http://127.0.0.1:1/x").json(&Version {
        version: "1".into(),
        meta: None,
    });
    let _ = patch; // just prove the builder type-checks
    println!("reqwest+serde ok: {}", v.version);
    Ok(())
}

fn yaml_smoke() -> anyhow::Result<()> {
    let s = "a: 1\nb:\n  - x\n  - y\n";
    let v: serde_json::Value = serde_norway::from_str(s)?;
    println!("serde_norway ok: {}", v["b"][1]);
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("cvt-core: {}", cvt_core::hello());
    yaml_smoke()?;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    let backend = TestBackend::new(80, 20);
    let mut term = Terminal::new(backend)?;
    term.draw(|f| cvt_tui::smoke_render(f, 1))?;
    let buf = term.backend().buffer().clone();
    println!("ratatui render ok: {}x{}", buf.area.width, buf.area.height);
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let k = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL);
    println!("crossterm ok: {:?}", k.code);
    let _ = tokio::spawn(async {});
    http_smoke().await?;
    Ok(())
}

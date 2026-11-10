//! End-to-end: agent ⇄ `x3 mcp` (stdio) ⇄ server `/mcp` (Streamable HTTP).

use std::{process::Stdio, sync::Arc};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use x3_server::{NewToken, Paths};

async fn rpc(
    stdin: &mut tokio::process::ChildStdin,
    out: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    msg: serde_json::Value,
) -> Option<serde_json::Value> {
    stdin
        .write_all(format!("{msg}\n").as_bytes())
        .await
        .unwrap();
    msg.get("id")?;
    loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(20), out.next_line())
            .await
            .expect("timeout waiting for shim")
            .unwrap()
            .expect("shim closed stdout");
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        if v.get("id") == msg.get("id") {
            return Some(v);
        }
    }
}

#[tokio::test]
async fn stdio_shim_forwards_to_server() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn x3_vault::SecretStore> = Arc::new(x3_vault::MemoryStore::new());
    let svc = x3_server::open_service(
        Paths::new(dir.path()),
        x3_server::default_model_router(store),
    )
    .await
    .unwrap();
    let (_, token) = svc
        .db()
        .mint_token(&NewToken {
            label: "shim-test".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let handle = x3_server::start(svc, 0).await.unwrap();

    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_x3"))
        .arg("mcp")
        .env("X3_TOKEN", &token)
        .env(
            "X3_URL",
            format!("http://127.0.0.1:{}/mcp", handle.addr.port()),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap()).lines();

    let init = rpc(
        &mut stdin,
        &mut out,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "0"}
            }
        }),
    )
    .await
    .unwrap();
    assert_eq!(init["result"]["serverInfo"]["name"], "voice-studio");
    rpc(
        &mut stdin,
        &mut out,
        serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )
    .await;

    let tools = rpc(
        &mut stdin,
        &mut out,
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    )
    .await
    .unwrap();
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for n in ["speak", "transcribe", "play", "list_models", "list_voices"] {
        assert!(names.contains(&n), "missing {n}");
    }

    let models = rpc(
        &mut stdin,
        &mut out,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "list_models", "arguments": {"task": "tts"}}
        }),
    )
    .await
    .unwrap();
    let text = models["result"]["content"][0]["text"].as_str().unwrap();
    let v: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(v["aliases"]["tts"], "apple/system");
    assert!(v["models"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["id"] == "elevenlabs/eleven_multilingual_v2" && m["available"] == false));

    // Cloud call without a key: a clear tool error, not a key prompt.
    let r = rpc(
        &mut stdin,
        &mut out,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": {"name": "speak", "arguments": {
                "text": "hi", "model": "elevenlabs/eleven_multilingual_v2", "play": false
            }}
        }),
    )
    .await
    .unwrap();
    assert_eq!(r["result"]["isError"], true);
    assert!(r["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("no API key configured"));

    drop(stdin);
    let _ = child.kill().await;
    handle.shutdown().await;
}

#[tokio::test]
async fn shim_rejects_non_x3_tokens() {
    let out = tokio::process::Command::new(env!("CARGO_BIN_EXE_x3"))
        .arg("mcp")
        .env("X3_TOKEN", "sk_live_provider_key")
        .stdin(Stdio::null())
        .output()
        .await
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("x3_live_"));
}

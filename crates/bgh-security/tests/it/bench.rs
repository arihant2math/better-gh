//! Push protection throughput on a 100 MB push (ignored by default):
//!
//! ```text
//! cargo test -p bgh-security --release --test it bench:: -- --ignored --nocapture
//! ```

use std::time::Instant;

use crate::common::*;

/// `n` bytes of source-like text (deterministic).
fn text(seed: u64, n: usize) -> String {
    const WORDS: &[&str] = &[
        "let",
        "value",
        "=",
        "compute(",
        "self.items",
        ")",
        "return",
        "if",
        "else",
        "{",
        "}",
        "for",
        "item",
        "in",
        "range(10)",
        "\"string literal\"",
        "// comment",
        "0x1f",
        "+",
        "*",
        "config",
        "token",
        "secret",
        "password",
        "key",
        "AKIA",
        "ghp_",
        "sk_live",
    ];
    let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    let mut s = String::with_capacity(n + 64);
    while s.len() < n {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.push_str(WORDS[(x % WORDS.len() as u64) as usize]);
        s.push(if x.is_multiple_of(11) { '\n' } else { ' ' });
    }
    s
}

#[tokio::test]
#[ignore]
async fn push_protection_100mb() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let work = seeded(&app, &alice, "big", &[("README.md", "hi\n")]).await;
    enable(&app, &alice, "alice/big", true).await;

    // 200 files x 512 KB = 100 MB, one secret in the last file.
    let files: Vec<(String, String)> = (0..200)
        .map(|i| {
            let mut t = text(i, 512 * 1024);
            if i == 199 {
                t.push_str(&format!("\naws = {AWS_KEY}\n"));
            }
            (format!("data/f{i:03}.txt"), t)
        })
        .collect();
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    work.commit(&refs, "100 MB").await;

    let t = Instant::now();
    let out = work.push("main").await;
    let blocked = t.elapsed();
    assert!(!out.ok, "{}", out.stderr);
    assert!(
        out.stderr.contains("path: data/f199.txt:"),
        "{}",
        out.stderr
    );

    // The same push without push protection (baseline: transfer + git).
    app.set_settings("secret_scanning", serde_json::json!({"available": false}))
        .await;
    let t = Instant::now();
    let out = work.push("main").await;
    let plain = t.elapsed();
    assert!(out.ok, "{}", out.stderr);
    println!(
        "100 MB push: {:.2}s with push protection (blocked), {:.2}s without; scan overhead ≈ {:.2}s",
        blocked.as_secs_f64(),
        plain.as_secs_f64(),
        blocked.as_secs_f64() - plain.as_secs_f64(),
    );
}

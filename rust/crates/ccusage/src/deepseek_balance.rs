fn is_deepseek_model(model_id: &str) -> bool {
    model_id.to_lowercase().contains("deepseek")
}

fn parse_balance(json: &str) -> Option<f64> {
    #[derive(serde::Deserialize)]
    struct BalanceResponse<'a> {
        #[serde(borrow)]
        balance_infos: Vec<BalanceInfo<'a>>,
    }
    #[derive(serde::Deserialize)]
    struct BalanceInfo<'a> {
        #[serde(borrow)]
        total_balance: &'a str,
    }
    let response: BalanceResponse = serde_json::from_str(json).ok()?;
    response.balance_infos.first()?.total_balance.parse().ok()
}

fn cache_fresh(fetched_at_ms: u64, now_ms: u64, ttl_ms: u64) -> bool {
    now_ms.saturating_sub(fetched_at_ms) <= ttl_ms
}

fn format_balance(cny: f64, warn_threshold: f64) -> String {
    if cny < warn_threshold {
        format!("⚠️ ¥{cny:.2}")
    } else {
        format!("💰 ¥{cny:.2}")
    }
}

use std::path::PathBuf;
use std::time::Duration;

const BALANCE_URL: &str = "https://api.deepseek.com/user/balance";
const BALANCE_FETCH_TIMEOUT_SECONDS: u64 = 3;

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn cache_path() -> PathBuf {
    std::env::temp_dir()
        .join("ccusage-semaphore")
        .join("deepseek_balance.json")
}

fn read_cache(path: &PathBuf) -> Option<(u64, f64)> {
    let bytes = std::fs::read(path).ok()?;
    #[derive(serde::Deserialize)]
    struct Cache {
        fetched_at_ms: u64,
        balance_cny: f64,
    }
    let cache: Cache = serde_json::from_slice(&bytes).ok()?;
    Some((cache.fetched_at_ms, cache.balance_cny))
}

fn write_cache(path: &PathBuf, fetched_at_ms: u64, balance_cny: f64) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let payload = serde_json::json!({ "fetched_at_ms": fetched_at_ms, "balance_cny": balance_cny });
    let _ = std::fs::write(path, serde_json::to_vec(&payload).unwrap_or_default());
}

fn fetch_balance(api_key: &str) -> Result<f64, String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(BALANCE_FETCH_TIMEOUT_SECONDS)))
        .build()
        .new_agent();
    let mut response = agent
        .get(BALANCE_URL)
        .header("Authorization", format!("Bearer {api_key}"))
        .call()
        .map_err(|error| error.to_string())?;
    if response.status().as_u16() != 200 {
        return Err(format!("HTTP {}", response.status().as_u16()));
    }
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|error| error.to_string())?;
    parse_balance(&body).ok_or_else(|| "invalid balance response".to_string())
}

fn balance_segment(model_id: &str, fetch: impl Fn(&str) -> Result<f64, String>) -> Option<String> {
    if !is_deepseek_model(model_id) {
        return None;
    }
    let api_key = std::env::var("DEEPSEEK_API_KEY").ok()?;
    let ttl_ms = std::env::var("DEEPSEEK_BALANCE_CACHE_TTL")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(3600)
        .saturating_mul(1000);
    let warn_threshold = std::env::var("DEEPSEEK_BALANCE_WARN")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(20.0);

    let now_ms = now_millis();
    let path = cache_path();
    let cached = read_cache(&path);
    let balance = match cached {
        Some((fetched_at, balance)) if cache_fresh(fetched_at, now_ms, ttl_ms) => Some(balance),
        _ => match fetch(&api_key) {
            Ok(balance) => {
                write_cache(&path, now_ms, balance);
                Some(balance)
            }
            Err(_) => cached.map(|(_, balance)| balance), // 降级:旧缓存兜底
        },
    };
    balance.map(|value| format_balance(value, warn_threshold))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deepseek_model_detection() {
        assert!(is_deepseek_model("deepseek-chat"));
        assert!(is_deepseek_model("DeepSeek-Reasoner"));
        assert!(is_deepseek_model("openrouter/deepseek/deepseek-r1"));
        assert!(!is_deepseek_model("claude-opus-4-5"));
        assert!(!is_deepseek_model(""));
    }

    #[test]
    fn parse_balance_extracts_total_balance() {
        let json = r#"{"is_available":true,"balance_infos":[{"currency":"CNY","total_balance":"110.00","granted_balance":"10.00","topped_up_balance":"100.00"}]}"#;
        assert_eq!(parse_balance(json), Some(110.0));
    }

    #[test]
    fn parse_balance_handles_malformed() {
        assert_eq!(parse_balance(r#"{"balance_infos":[]}"#), None);
        assert_eq!(parse_balance(r#"{"is_available":true}"#), None);
        assert_eq!(parse_balance(r#"{"balance_infos":[{"currency":"CNY","total_balance":"abc"}]}"#), None);
        assert_eq!(parse_balance("not json"), None);
    }

    #[test]
    fn cache_freshness_window() {
        let ttl_ms = 3600 * 1000;
        assert!(cache_fresh(1_000, 60_000, ttl_ms));
        assert!(!cache_fresh(1_000, 1_000 + ttl_ms + 1, ttl_ms));
        assert!(cache_fresh(1_000, 1_000 + ttl_ms, ttl_ms)); // 恰好 ttl 边界算新鲜
    }

    #[test]
    fn format_balance_threshold() {
        assert_eq!(format_balance(110.0, 20.0), "💰 ¥110.00");
        assert_eq!(format_balance(20.0, 20.0), "💰 ¥20.00");  // >= 阈值 → 💰
        assert_eq!(format_balance(5.0, 20.0), "⚠️ ¥5.00");
    }

    #[test]
    fn segment_skipped_for_non_deepseek_model() {
        assert_eq!(
            balance_segment("claude-opus-4-5", |_| panic!("fetch must not be called")),
            None
        );
    }

    #[test]
    fn segment_uses_fresh_cache_without_network() {
        // EnvVarGuard holds a non-reentrant global mutex: at most one per test.
        let _key = ccusage_test_support::EnvVarGuard::set("DEEPSEEK_API_KEY", "test-key");
        // SAFETY: no other test reads these vars, so removal cannot race.
        unsafe { std::env::remove_var("DEEPSEEK_BALANCE_CACHE_TTL") };
        unsafe { std::env::remove_var("DEEPSEEK_BALANCE_WARN") };
        let path = cache_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let payload = serde_json::json!({
            "fetched_at_ms": now_millis(),
            "balance_cny": 110.0
        });
        std::fs::write(&path, serde_json::to_vec(&payload).unwrap()).unwrap();
        // 默认 ttl=3600s,刚写入的时间戳 → 缓存恒新鲜;panic 闭包结构性证明不发网络
        assert_eq!(
            balance_segment("deepseek-chat", |_| panic!("fetch must not be called")),
            Some("💰 ¥110.00".to_string())
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn cache_roundtrip() {
        // 唯一临时路径,避免与 segment_uses_fresh_cache_without_network 并行时互踩真实缓存文件
        let path = std::env::temp_dir().join(format!("ccusage-balance-test-{}", std::process::id()));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_cache(&path, 1_234, 88.5);
        assert_eq!(read_cache(&path), Some((1_234, 88.5)));
        std::fs::remove_file(&path).ok();
    }
}

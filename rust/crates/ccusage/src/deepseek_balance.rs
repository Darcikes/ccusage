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

#[derive(serde::Deserialize, serde::Serialize, PartialEq, Debug)]
struct BalanceCache {
    fetched_at_ms: u64,
    balance_cny: Option<f64>,
    #[serde(default)]
    failed_at_ms: Option<u64>,
}

fn read_cache(path: &PathBuf) -> Option<BalanceCache> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_cache(
    path: &PathBuf,
    fetched_at_ms: u64,
    balance_cny: Option<f64>,
    failed_at_ms: Option<u64>,
) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let cache = BalanceCache {
        fetched_at_ms,
        balance_cny,
        failed_at_ms,
    };
    let _ = std::fs::write(path, serde_json::to_vec(&cache).unwrap_or_default());
}

pub(crate) fn fetch_balance(api_key: &str) -> Result<f64, String> {
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

pub(crate) fn balance_segment(
    model_id: &str,
    fetch: impl Fn(&str) -> Result<f64, String>,
) -> Option<String> {
    // 总开关:仅 CCUSAGE_BALANCE_ENABLED=true(忽略大小写)时启用;缺省或 false → 静默关闭
    let enabled = std::env::var("CCUSAGE_BALANCE_ENABLED")
        .map(|value| value.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if !enabled {
        return None;
    }
    if !is_deepseek_model(model_id) {
        return None;
    }
    // 多账户:优先用 ANTHROPIC_AUTH_TOKEN(alias 注入的当前账户 key),DEEPSEEK_API_KEY 回退
    let api_key = std::env::var("ANTHROPIC_AUTH_TOKEN")
        .or_else(|_| std::env::var("DEEPSEEK_API_KEY"))
        .ok()?;
    let ttl_ms = std::env::var("CCUSAGE_BALANCE_CACHE_TTL")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(3600)
        .saturating_mul(1000);
    let warn_threshold = std::env::var("CCUSAGE_BALANCE_WARN")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(20.0);

    let now_ms = now_millis();
    let path = cache_path();
    let cached = read_cache(&path);
    // 有失败标记用 failed_at_ms 节流重试,否则用 fetched_at_ms(原逻辑)
    let balance = match cached.as_ref() {
        Some(cache) if cache_fresh(cache.failed_at_ms.unwrap_or(cache.fetched_at_ms), now_ms, ttl_ms) => {
            cache.balance_cny
        }
        _ => match fetch(&api_key) {
            Ok(balance) => {
                write_cache(&path, now_ms, Some(balance), None);
                Some(balance)
            }
            Err(_) => {
                let stale = cached.as_ref().and_then(|cache| cache.balance_cny);
                write_cache(&path, now_ms, stale, Some(now_ms)); // 失败也写缓存,节流期内不重试
                stale
            }
        },
    };
    balance.map(|value| format_balance(value, warn_threshold))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::Mutex;

    // 写同一缓存文件(cache_path)的测试共用此锁,避免并行互踩
    static CACHE_LOCK: Mutex<()> = Mutex::new(());

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
        // EnvVarGuard holds a non-reentrant global mutex: at most one per test.
        let _enabled = ccusage_test_support::EnvVarGuard::set("CCUSAGE_BALANCE_ENABLED", "true");
        assert_eq!(
            balance_segment("claude-opus-4-5", |_| panic!("fetch must not be called")),
            None
        );
    }

    #[test]
    fn segment_prefers_anthropic_auth_token() {
        // EnvVarGuard holds a non-reentrant global mutex: at most one per test.
        let _enabled = ccusage_test_support::EnvVarGuard::set("CCUSAGE_BALANCE_ENABLED", "true");
        // SAFETY: no other test reads these vars, so removal cannot race.
        unsafe { std::env::set_var("ANTHROPIC_AUTH_TOKEN", "anthropic-key") };
        unsafe { std::env::set_var("DEEPSEEK_API_KEY", "deepseek-key") };
        unsafe { std::env::remove_var("CCUSAGE_BALANCE_CACHE_TTL") };
        unsafe { std::env::remove_var("CCUSAGE_BALANCE_WARN") };
        let _cache = CACHE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let path = cache_path();
        std::fs::remove_file(&path).ok();
        let seen = Cell::new(None);
        let fetch = |key: &str| {
            seen.replace(Some(key.to_string()));
            Ok(88.5)
        };
        assert_eq!(
            balance_segment("deepseek-chat", fetch),
            Some("💰 ¥88.50".to_string())
        );
        // 两个 key 都存在时,传给 fetch 的必须是 ANTHROPIC_AUTH_TOKEN
        assert_eq!(seen.replace(None).as_deref(), Some("anthropic-key"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn segment_disabled_when_switch_off() {
        // EnvVarGuard holds a non-reentrant global mutex: at most one per test.
        let _enabled = ccusage_test_support::EnvVarGuard::set("CCUSAGE_BALANCE_ENABLED", "false");
        // 开关关:即使有 key + deepseek 模型,也不请求、不显示(开关短路,不读 key)
        assert_eq!(
            balance_segment("deepseek-chat", |_| panic!("fetch must not be called")),
            None
        );
    }

    #[test]
    fn segment_disabled_when_switch_missing() {
        // 缺省开关 = 关闭。并行下无法可靠 remove_var(会删掉其他测试 guard 设的值),
        // 用 guard 设空值模拟"未开启":与缺省一样走 unwrap_or(false) → 不请求、不显示
        let _enabled = ccusage_test_support::EnvVarGuard::set("CCUSAGE_BALANCE_ENABLED", "");
        assert_eq!(
            balance_segment("deepseek-chat", |_| panic!("fetch must not be called")),
            None
        );
    }

    #[test]
    fn segment_uses_fresh_cache_without_network() {
        // EnvVarGuard holds a non-reentrant global mutex: at most one per test.
        let _enabled = ccusage_test_support::EnvVarGuard::set("CCUSAGE_BALANCE_ENABLED", "true");
        // SAFETY: no other test reads these vars, so removal cannot race.
        unsafe { std::env::set_var("DEEPSEEK_API_KEY", "test-key") };
        // ANTHROPIC_AUTH_TOKEN 优先于 DEEPSEEK_API_KEY:显式清掉,防止外部 token 污染本测试
        unsafe { std::env::remove_var("ANTHROPIC_AUTH_TOKEN") };
        unsafe { std::env::remove_var("CCUSAGE_BALANCE_CACHE_TTL") };
        unsafe { std::env::remove_var("CCUSAGE_BALANCE_WARN") };
        // 锁内断言失败会毒化锁:空元组锁无数据,毒化无害,直接接管
        let _cache = CACHE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        write_cache(&path, 1_234, Some(88.5), Some(5_678));
        assert_eq!(
            read_cache(&path),
            Some(BalanceCache {
                fetched_at_ms: 1_234,
                balance_cny: Some(88.5),
                failed_at_ms: Some(5_678),
            })
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn failure_writes_cache_and_throttles_retries() {
        // EnvVarGuard holds a non-reentrant global mutex: at most one per test.
        let _enabled = ccusage_test_support::EnvVarGuard::set("CCUSAGE_BALANCE_ENABLED", "true");
        // SAFETY: no other test reads these vars, so removal cannot race.
        unsafe { std::env::set_var("DEEPSEEK_API_KEY", "test-key") };
        // ANTHROPIC_AUTH_TOKEN 优先于 DEEPSEEK_API_KEY:显式清掉,防止外部 token 污染本测试
        unsafe { std::env::remove_var("ANTHROPIC_AUTH_TOKEN") };
        unsafe { std::env::remove_var("CCUSAGE_BALANCE_CACHE_TTL") };
        unsafe { std::env::remove_var("CCUSAGE_BALANCE_WARN") };
        // 锁内断言失败会毒化锁:空元组锁无数据,毒化无害,直接接管
        let _cache = CACHE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let path = cache_path();
        std::fs::remove_file(&path).ok();
        let calls = Cell::new(0);
        let fetch = |_api_key: &str| {
            calls.set(calls.get() + 1);
            Err("network down".to_string())
        };
        // 无旧值可显示 → 隐藏
        assert_eq!(balance_segment("deepseek-chat", fetch), None);
        assert_eq!(calls.get(), 1);
        // 失败标记已写盘,TTL 内第二次调用不重试
        assert!(read_cache(&path).is_some_and(|c| c.failed_at_ms.is_some()));
        assert_eq!(balance_segment("deepseek-chat", fetch), None);
        assert_eq!(calls.get(), 1);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn failure_keeps_showing_stale_balance() {
        // EnvVarGuard holds a non-reentrant global mutex: at most one per test.
        let _enabled = ccusage_test_support::EnvVarGuard::set("CCUSAGE_BALANCE_ENABLED", "true");
        // SAFETY: no other test reads these vars, so removal cannot race.
        unsafe { std::env::set_var("DEEPSEEK_API_KEY", "test-key") };
        // ANTHROPIC_AUTH_TOKEN 优先于 DEEPSEEK_API_KEY:显式清掉,防止外部 token 污染本测试
        unsafe { std::env::remove_var("ANTHROPIC_AUTH_TOKEN") };
        unsafe { std::env::remove_var("CCUSAGE_BALANCE_CACHE_TTL") };
        unsafe { std::env::remove_var("CCUSAGE_BALANCE_WARN") };
        // 锁内断言失败会毒化锁:空元组锁无数据,毒化无害,直接接管
        let _cache = CACHE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let path = cache_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // 过期的成功缓存(纪元后 1s,距今远超默认 TTL)→ 尝试 fetch → 失败 → 显示旧值
        write_cache(&path, 1_000, Some(88.5), None);
        let calls = Cell::new(0);
        let fetch = |_api_key: &str| {
            calls.set(calls.get() + 1);
            Err("network down".to_string())
        };
        assert_eq!(
            balance_segment("deepseek-chat", fetch),
            Some("💰 ¥88.50".to_string())
        );
        assert_eq!(calls.get(), 1);
        // 失败节流期内持续显示陈旧值
        assert_eq!(
            balance_segment("deepseek-chat", fetch),
            Some("💰 ¥88.50".to_string())
        );
        assert_eq!(calls.get(), 1);
        std::fs::remove_file(&path).ok();
    }
}

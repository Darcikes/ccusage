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
}

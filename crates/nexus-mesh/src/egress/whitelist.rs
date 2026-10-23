//! 出站目标严格域名白名单与安全校验器。

use std::net::IpAddr;

/// 出站安全检查器。
#[derive(Debug, Clone)]
pub struct WhitelistChecker {
    allowed_domains: Vec<String>,
}

impl WhitelistChecker {
    pub fn new(allowed_domains: Vec<String>) -> Self {
        Self { allowed_domains }
    }

    /// 校验目标主机与端口是否被允许。
    pub fn is_allowed(&self, host: &str, port: u16) -> Result<(), &'static str> {
        // 严格端口限制：AI 官方 API 服务只允许 443 (HTTPS)
        if port != 443 && port != 8443 {
            return Err("出站端口受限：只允许 443/8443 (HTTPS)");
        }

        let host_trimmed = host.trim().to_ascii_lowercase();

        // 拦截私有网络与本地回环，防止 SSRF 或探测内网
        if host_trimmed == "localhost" || host_trimmed.ends_with(".local") {
            return Err("禁止访问本地回环或内网主机");
        }

        // 如果是直接传入的 IP 地址，检查是否为私网 IP
        if let Ok(ip) = host_trimmed.parse::<IpAddr>() {
            if is_private_or_loopback(&ip) {
                return Err("禁止访问私有 IP 地址或回环地址");
            }
        }

        // 匹配域名白名单 (支持前缀泛匹配与精确匹配)
        let matched = self.allowed_domains.iter().any(|rule| {
            let rule = rule.trim().to_ascii_lowercase();
            if rule.starts_with("*.") {
                let suffix = &rule[2..];
                host_trimmed == suffix || host_trimmed.ends_with(&format!(".{}", suffix))
            } else {
                host_trimmed == rule
            }
        });

        if matched {
            Ok(())
        } else {
            Err("目标域名不在 AI 官方服务白名单中")
        }
    }
}

fn is_private_or_loopback(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_unspecified()
        }
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unspecified(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_whitelist_allows_ai_domains() {
        let checker = WhitelistChecker::new(vec![
            "*.claude.ai".to_string(),
            "api.openai.com".to_string(),
            "chatgpt.com".to_string(),
        ]);

        assert!(checker.is_allowed("api.claude.ai", 443).is_ok());
        assert!(checker.is_allowed("claude.ai", 443).is_ok());
        assert!(checker.is_allowed("api.openai.com", 443).is_ok());
        assert!(checker.is_allowed("chatgpt.com", 443).is_ok());

        // 非白名单域名
        assert!(checker.is_allowed("google.com", 443).is_err());
        assert!(checker.is_allowed("baidu.com", 443).is_err());

        // 非安全端口
        assert!(checker.is_allowed("api.claude.ai", 80).is_err());
        assert!(checker.is_allowed("api.claude.ai", 22).is_err());

        // 内网与本地回环
        assert!(checker.is_allowed("127.0.0.1", 443).is_err());
        assert!(checker.is_allowed("192.168.1.1", 443).is_err());
        assert!(checker.is_allowed("localhost", 443).is_err());
    }
}
